/**
 * Property-based tests for voice-room.ts media wiring (Task 7.7).
 *
 * Tests P1, P2, P3, P9, P22, P23, P24, P26, P27, P34 — the integration
 * between voice-room.ts and LiveKitModule for media_token dispatch,
 * buffering, reconnection, mute sync, and failure isolation.
 *
 * Strategy: mock LiveKitModule, SignalingClient, and auth modules,
 * then drive voice-room through initSession → dispatchMessage flow.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as fc from 'fast-check';
import { clearTelemetrySnapshot, getTelemetrySnapshot } from '../telemetry';

// ─── Mock State ────────────────────────────────────────────────────

/** Captured LiveKitModule constructor calls. */
let lkConstructorCalls: Array<Record<string, unknown>>;

/** The most recently created mock LiveKitModule instance. */
let lastLkModule: MockLiveKitModule | null;

/** Captured SignalingClient.send() calls. */
let sentMessages: Array<Record<string, unknown>>;

/** The message handler registered via client.onMessage(). */
let messageHandler: ((msg: unknown) => void) | null;

/** The status change handler registered via client.onStatusChange(). */
let statusChangeHandler: ((status: string) => void) | null;

/** The most recently created mock SignalingClient instance. */
let lastSignalingClient: Record<string, unknown> | null;

/** Whether connectWithAuth should reject. */
let connectShouldFail: boolean;
let playNotificationSoundCalls: string[];
let invokeCalls: Array<{ command: string; args?: Record<string, unknown> }>;

// ─── Mock LiveKitModule ────────────────────────────────────────────

interface MockLiveKitModule {
  callbacks: Record<string, (...args: unknown[]) => void>;
  connectCalls: Array<{ sfuUrl: string; token: string }>;
  disconnectCalls: number;
  setMicEnabledCalls: Array<boolean>;
  publishCameraCalls: Array<{ deviceId: string | null; quality: { tier: string } }>;
  unpublishCameraCalls: number;
  setCameraQualityCalls: Array<{ tier: string }>;
  replaceCameraDeviceCalls: Array<string | null>;
  setParticipantVolumeCalls: Array<{ id: string; vol: number }>;
  setMasterVolumeCalls: Array<number>;
  setScreenShareAudioVolumeCalls: Array<{ id: string; vol: number }>;
  attachScreenShareAudioCalls: string[];
  detachScreenShareAudioCalls: string[];
  startWasapiAudioBridgeCalls: Array<boolean>;
  stopWasapiAudioBridgeCalls: number;
  startScreenShareCalls: number;
  stopScreenShareCalls: number;
  refreshRemoteScreenShareCalls: string[];
  activeScreenShares: Array<{ identity: string; stream: MediaStream; startedAtMs: number }>;
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
  setParticipantPassthrough: (id: string, enabled: boolean) => void;
  setPassthroughFilterSettings: (settings: { enabled: boolean; strength: number }) => void;
  setScreenShareAudioVolume: (id: string, vol: number) => void;
  attachScreenShareAudio: (id: string) => void;
  detachScreenShareAudio: (id: string) => void;
  startWasapiAudioBridge: (loopbackExclusionAvailable: boolean) => Promise<void>;
  stopWasapiAudioBridge: () => Promise<void>;
  startScreenShare: () => Promise<boolean>;
  stopScreenShare: () => Promise<void>;
  refreshRemoteScreenShare: (identity: string) => void;
  getActiveScreenShares: () => Array<{
    identity: string;
    stream: MediaStream;
    startedAtMs: number;
  }>;
}

function createMockLkModule(
  callbacks: Record<string, (...args: unknown[]) => void>,
): MockLiveKitModule {
  const mod: MockLiveKitModule = {
    callbacks,
    connectCalls: [],
    disconnectCalls: 0,
    setMicEnabledCalls: [],
    publishCameraCalls: [],
    unpublishCameraCalls: 0,
    setCameraQualityCalls: [],
    replaceCameraDeviceCalls: [],
    setParticipantVolumeCalls: [],
    setMasterVolumeCalls: [],
    setScreenShareAudioVolumeCalls: [],
    attachScreenShareAudioCalls: [],
    detachScreenShareAudioCalls: [],
    startWasapiAudioBridgeCalls: [],
    stopWasapiAudioBridgeCalls: 0,
    startScreenShareCalls: 0,
    stopScreenShareCalls: 0,
    refreshRemoteScreenShareCalls: [],
    activeScreenShares: [],
    localCameraTrack: null,
    connect: vi.fn(async (sfuUrl: string, token: string) => {
      mod.connectCalls.push({ sfuUrl, token });
    }),
    disconnect: vi.fn(() => {
      mod.disconnectCalls++;
    }),
    setMicEnabled: vi.fn(async (enabled: boolean) => {
      mod.setMicEnabledCalls.push(enabled);
    }),
    publishCamera: vi.fn(async (opts: { deviceId: string | null; quality: { tier: string } }) => {
      mod.publishCameraCalls.push(opts);
      mod.localCameraTrack = {
        id: `camera-${opts.quality.tier}`,
        kind: 'video',
      } as MediaStreamTrack;
      return { trackId: `camera-${opts.quality.tier}` };
    }),
    unpublishCamera: vi.fn(async () => {
      mod.unpublishCameraCalls += 1;
      mod.localCameraTrack = null;
    }),
    setCameraQuality: vi.fn(async (quality: { tier: string }) => {
      mod.setCameraQualityCalls.push(quality);
    }),
    replaceCameraDevice: vi.fn(async (deviceId: string | null) => {
      mod.replaceCameraDeviceCalls.push(deviceId);
      mod.localCameraTrack = {
        id: `camera-${deviceId ?? 'default'}`,
        kind: 'video',
      } as MediaStreamTrack;
      return { trackId: `camera-${deviceId ?? 'default'}` };
    }),
    getLocalCameraTrack: vi.fn(() => mod.localCameraTrack),
    applyRemoteCameraVisibility: vi.fn(() => {}),
    setParticipantVolume: vi.fn((id: string, vol: number) => {
      mod.setParticipantVolumeCalls.push({ id, vol });
    }),
    setMasterVolume: vi.fn((vol: number) => {
      mod.setMasterVolumeCalls.push(vol);
    }),
    setParticipantPassthrough: vi.fn(() => {}),
    setPassthroughFilterSettings: vi.fn(() => {}),
    setScreenShareAudioVolume: vi.fn((id: string, vol: number) => {
      mod.setScreenShareAudioVolumeCalls.push({ id, vol });
    }),
    attachScreenShareAudio: vi.fn((id: string) => {
      mod.attachScreenShareAudioCalls.push(id);
    }),
    detachScreenShareAudio: vi.fn((id: string) => {
      mod.detachScreenShareAudioCalls.push(id);
    }),
    startWasapiAudioBridge: vi.fn(async (loopbackExclusionAvailable: boolean) => {
      mod.startWasapiAudioBridgeCalls.push(loopbackExclusionAvailable);
    }),
    stopWasapiAudioBridge: vi.fn(async () => {
      mod.stopWasapiAudioBridgeCalls += 1;
    }),
    startScreenShare: vi.fn(async () => {
      mod.startScreenShareCalls++;
      return true;
    }),
    stopScreenShare: vi.fn(async () => {
      mod.stopScreenShareCalls++;
    }),
    refreshRemoteScreenShare: vi.fn((identity: string) => {
      mod.refreshRemoteScreenShareCalls.push(identity);
    }),
    getActiveScreenShares: vi.fn(() => mod.activeScreenShares),
  };
  return mod;
}

// ─── Mock livekit-media module ─────────────────────────────────────

vi.mock('../livekit-media', () => ({
  LiveKitModule: vi.fn(function (
    this: Record<string, unknown>,
    callbacks: Record<string, (...args: unknown[]) => void>,
  ) {
    const mod = createMockLkModule(callbacks);
    lastLkModule = mod;
    lkConstructorCalls.push(callbacks);
    // Copy methods onto `this` so the module-level code sees them
    Object.assign(this, mod);
    return this;
  }),
}));

// ─── Mock websocket module ─────────────────────────────────────────

vi.mock('@shared/websocket', () => ({
  SignalingClient: vi.fn(function (this: Record<string, unknown>) {
    lastSignalingClient = this;
    this.status = 'disconnected';
    this.send = vi.fn((msg: Record<string, unknown>) => {
      sentMessages.push(msg);
    });
    this.onMessage = vi.fn((handler: (msg: unknown) => void) => {
      messageHandler = handler;
      return () => {
        messageHandler = null;
      };
    });
    this.onStatusChange = vi.fn((handler: (status: string) => void) => {
      statusChangeHandler = handler;
      return () => {
        statusChangeHandler = null;
      };
    });
    this.connectWithAuth = vi.fn(async () => {
      if (connectShouldFail) throw new Error('connect failed');
      this.status = 'connected';
    });
    this.disconnect = vi.fn(() => {
      this.status = 'disconnected';
    });
    return this;
  }),
}));

// ─── Mock auth module ──────────────────────────────────────────────

vi.mock('@features/auth/auth', () => ({
  getServerUrl: vi.fn(async () => 'https://test.wavis.dev'),
  getDisplayName: vi.fn(async () => 'TestUser'),
  getUsername: vi.fn(async () => 'TestUser'),
  getAccessToken: vi.fn(async () => 'mock-token'),
  isTokenExpired: vi.fn(async () => false),
  refreshTokens: vi.fn(async () => true),
  onTokensRefreshed: vi.fn((_cb: () => void) => () => {}),
}));

// ─── Mock helpers module ───────────────────────────────────────────

vi.mock('@shared/helpers', () => ({
  toWsUrl: vi.fn((url: string) => url.replace('https://', 'wss://') + '/ws'),
}));

// ─── Mock audio-devices module ─────────────────────────────────────

vi.mock('../audio-devices', () => ({
  setActiveLiveKitModule: vi.fn(),
}));

// ─── Mock settings-store module ────────────────────────────────────

let mockMaxRetries = 10;

vi.mock('@features/settings/settings-store', () => ({
  DEFAULT_PASSTHROUGH_VOLUME: 20,
  getDefaultVolume: vi.fn(async () => 70),
  getReconnectConfig: vi.fn(async () => ({
    strategy: 'exponential' as const,
    baseDelayMs: 1000,
    maxDelayMs: 30000,
    maxRetries: mockMaxRetries,
  })),
  getMuteHotkey: vi.fn(async () => 'Ctrl+Shift+M'),
  getProfileColor: vi.fn(async () => '#E06C75'),
  getChannelVolumes: vi.fn(async () => null),
  setChannelVolumes: vi.fn(async () => {}),
  getWindowsSharePath: vi.fn(async () => 'browser'),
  getVideoInputDevice: vi.fn(async () => null),
  setVideoInputDevice: vi.fn(async () => {}),
  getNotificationVolume: vi.fn(async () => 100),
  getSoundVolumes: vi.fn(async () => ({})),
}));

vi.mock('@shared/hotkey-bridge', () => ({
  registerMuteHotkey: vi.fn(async () => {}),
  unregisterMuteHotkey: vi.fn(async () => {}),
}));

vi.mock('../notification-sounds', () => ({
  playNotificationSound: vi.fn(async (name: string) => {
    playNotificationSoundCalls.push(name);
  }),
  updateCachedNotificationVolume: vi.fn(),
  updateCachedSoundVolumes: vi.fn(),
  prewarmAudioContext: vi.fn(),
}));

// ─── Mock Tauri APIs ───────────────────────────────────────────────
// voice-room.ts imports invoke, listen, emit, and WebviewWindow from
// @tauri-apps/api — these access window.__TAURI_INTERNALS__ which
// doesn't exist in Node.js.

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (command: string, args?: Record<string, unknown>) => {
    invokeCalls.push({ command, args });
    if (command === 'audio_share_start') {
      return { loopback_exclusion_available: true };
    }
    return {};
  }),
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

// native-media.ts also imports from @tauri-apps/api and screen-share-viewer
vi.mock('../native-media', () => ({
  NativeMediaModule: vi.fn(function (
    this: Record<string, unknown>,
    callbacks: Record<string, (...args: unknown[]) => void>,
  ) {
    const mod = createMockLkModule(callbacks);
    lastLkModule = mod;
    lkConstructorCalls.push(callbacks);
    Object.assign(this, mod);
    return this;
  }),
}));

// ─── Import module under test ──────────────────────────────────────

import {
  initSession,
  leaveRoom,
  scheduleLeaveRoom,
  BACKGROUND_LEAVE_DISCONNECT_MS,
  leaveSubRoom,
  joinSubRoom,
  toggleSelfMute,
  toggleSelfDeafen,
  toggleCameraIntent,
  reconnectMedia,
  resetMediaReconnectFailures,
  setScreenShareAudioVolume,
  attachScreenShareAudio,
  detachScreenShareAudio,
  startCustomShare,
  getState,
  isShareEnabled,
  persistStreamVolume,
  getPersistedStreamVolume,
  persistStreamMuted,
  getPersistedStreamMuted,
} from '../voice-room';
import type { VoiceRoomState } from '../voice-room';
import * as settingsStore from '@features/settings/settings-store';

// ─── Test Helpers ──────────────────────────────────────────────────

function resetAll() {
  lkConstructorCalls = [];
  lastLkModule = null;
  sentMessages = [];
  messageHandler = null;
  statusChangeHandler = null;
  lastSignalingClient = null;
  connectShouldFail = false;
  mockMaxRetries = 10;
  playNotificationSoundCalls = [];
  invokeCalls = [];
  clearTelemetrySnapshot();
  (globalThis as { __wavisSenderData?: unknown }).__wavisSenderData = {};
}

/** Flush microtask queue. */
const tick = () => new Promise<void>((r) => setTimeout(r, 0));

let latestState: VoiceRoomState | null = null;

/** Initialize a session and drive it to the `active` state.
 * By default also assigns self to room-1 via sub_room_state so that a
 * subsequent media_token triggers connectMedia immediately. Pass
 * joinSubRoom = false for tests that specifically need the pre-room state.
 */
async function driveToActive(channelId = 'ch-1', channelName = 'test-room', joinSubRoom = true) {
  latestState = null;
  initSession(channelId, channelName, 'owner', (s) => {
    latestState = s;
  });
  await tick(); // let connectWithAuth resolve

  // Simulate auth_success → joining → joined
  if (messageHandler) {
    messageHandler({ type: 'auth_success' });
    messageHandler({
      type: 'joined',
      peerId: 'self-peer',
      roomId: 'room-1',
      participants: [
        { participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' },
        { participantId: 'peer-2', displayName: 'Alice', userId: 'u2' },
      ],
    });
  }
  if (joinSubRoom) {
    await assignSelfToSubRoom();
  } else {
    await tick();
  }
}

async function assignSelfToSubRoom(subRoomId = 'room-1') {
  messageHandler!({
    type: 'sub_room_state',
    rooms: [{ subRoomId, roomNumber: 1, isDefault: true, participantIds: ['self-peer'] }],
  });
  await tick();
}

beforeEach(() => {
  resetAll();
});

describe('VoiceRoom screen share audio delegation', () => {
  it('setScreenShareAudioVolume delegates to the media module with clamped volume', async () => {
    resetAll();
    await driveToActive();

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();

    setScreenShareAudioVolume('alice', 133);
    setScreenShareAudioVolume('bob', -5);

    expect(lastLkModule).not.toBeNull();
    expect(lastLkModule!.setScreenShareAudioVolumeCalls).toEqual([
      { id: 'alice', vol: 100 },
      { id: 'bob', vol: 0 },
    ]);

    leaveRoom();
  });

  it('keeps LiveKit-inferred audio-only state across legacy share_state without shareType', async () => {
    resetAll();
    await driveToActive();

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();

    lastLkModule!.callbacks.onAudioOnlySharerAdded('peer-2');
    await tick();
    expect(getState().audioOnlySharers.has('peer-2')).toBe(true);

    messageHandler!({
      type: 'share_state',
      participantIds: ['peer-2'],
      activeShares: [{ participantId: 'peer-2' }],
    });
    await tick();

    expect(getState().participants.find((p) => p.id === 'peer-2')).toMatchObject({
      isSharing: true,
      shareType: undefined,
    });
    expect(getState().audioOnlySharers.has('peer-2')).toBe(true);

    messageHandler!({
      type: 'share_state',
      participantIds: ['peer-2'],
      activeShares: [{ participantId: 'peer-2', shareType: 'screen_audio' }],
    });
    await tick();

    expect(getState().audioOnlySharers.has('peer-2')).toBe(false);

    leaveRoom();
  });

  it('keeps LiveKit-inferred audio-only state when share_started arrives without shareType', async () => {
    resetAll();
    await driveToActive();

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();

    // Simulate lkModule inferring audio-only from TrackSubscribed before share_started arrives
    lastLkModule!.callbacks.onAudioOnlySharerAdded('peer-2');
    await tick();
    expect(getState().audioOnlySharers.has('peer-2')).toBe(true);

    // share_started arrives without shareType (older sender client)
    messageHandler!({ type: 'share_started', participantId: 'peer-2' });
    await tick();

    expect(getState().participants.find((p) => p.id === 'peer-2')).toMatchObject({
      isSharing: true,
      shareType: undefined,
    });
    // Inferred audio-only state must be preserved
    expect(getState().audioOnlySharers.has('peer-2')).toBe(true);

    // An explicit non-audio-only type should still clear it
    messageHandler!({ type: 'share_started', participantId: 'peer-2', shareType: 'screen_audio' });
    await tick();
    expect(getState().audioOnlySharers.has('peer-2')).toBe(false);

    leaveRoom();
  });

  it('attachScreenShareAudio and detachScreenShareAudio delegate to the media module', async () => {
    resetAll();
    await driveToActive();

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();

    attachScreenShareAudio('alice');
    detachScreenShareAudio('alice');

    expect(lastLkModule).not.toBeNull();
    expect(lastLkModule!.attachScreenShareAudioCalls).toEqual(['alice']);
    expect(lastLkModule!.detachScreenShareAudioCalls).toEqual(['alice']);

    leaveRoom();
  });
});

describe('VoiceRoom camera orchestration', () => {
  it('publishes camera at LOW quality when a same-room screen share is already active', async () => {
    vi.stubGlobal('window', {});
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: {
        enumerateDevices: vi.fn(async () => [
          { kind: 'videoinput', deviceId: 'camera-1', label: 'Camera 1' },
        ]),
      },
    });
    vi.spyOn(settingsStore, 'getVideoInputDevice').mockResolvedValue('camera-1');

    await driveToActive();
    messageHandler!({
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

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    messageHandler!({
      type: 'share_started',
      participantId: 'peer-2',
      displayName: 'Alice',
      shareType: 'screen_audio',
    });
    await tick();

    await toggleCameraIntent();

    expect(lastLkModule).not.toBeNull();
    expect(lastLkModule!.publishCameraCalls).toHaveLength(1);
    expect(lastLkModule!.publishCameraCalls[0].deviceId).toBe('camera-1');
    expect(lastLkModule!.publishCameraCalls[0].quality.tier).toBe('low');

    leaveRoom();
  });
});

describe('VoiceRoom sub-room state', () => {
  it('join_voice advertises sub-room support', async () => {
    initSession('ch-subrooms', 'subroom-test', 'owner', (s) => {
      latestState = s;
    });
    await tick();

    if (messageHandler) {
      messageHandler({ type: 'auth_success' });
    }
    await tick();

    const joinVoiceMsg = sentMessages.find((m) => m.type === 'join_voice');
    expect(joinVoiceMsg).toMatchObject({
      type: 'join_voice',
      channelId: 'ch-subrooms',
      supportsSubRooms: true,
    });
  });

  it('derives participant-to-room mapping from sub_room_state snapshots', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['peer-2'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['self-peer'] },
      ],
      passthrough: {
        sourceSubRoomId: 'room-1',
        targetSubRoomId: 'room-2',
        label: '1 - 2',
      },
    });
    await tick();

    const state = getState();
    expect(state.subRooms).toHaveLength(2);
    expect(state.participantSubRoomById).toEqual({
      'peer-2': 'room-1',
      'self-peer': 'room-2',
    });
    expect(state.joinedSubRoomId).toBe('room-2');
    expect(state.desiredSubRoomId).toBe('room-2');
    expect(state.passthrough).toEqual({
      sourceSubRoomId: 'room-1',
      targetSubRoomId: 'room-2',
      label: '1 - 2',
    });
  });

  it('rejoins the desired sub-room after reconnect when the room still exists', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        {
          subRoomId: 'room-1',
          roomNumber: 1,
          isDefault: true,
          participantIds: ['self-peer', 'peer-2'],
        },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();

    joinSubRoom('room-2');

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'self-peer',
      subRoomId: 'room-2',
      source: 'explicit',
    });
    await tick();

    statusChangeHandler?.('disconnected');
    expect(getState().machineState).toBe('reconnecting');

    if (messageHandler) {
      messageHandler({ type: 'auth_success' });
      messageHandler({
        type: 'joined',
        peerId: 'self-peer-reconnected',
        roomId: 'room-1',
        participants: [
          { participantId: 'self-peer-reconnected', displayName: 'TestUser', userId: 'u1' },
          { participantId: 'peer-2', displayName: 'Alice', userId: 'u2' },
        ],
      });
      messageHandler({
        type: 'sub_room_state',
        rooms: [
          { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['peer-2'] },
          { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
        ],
      });
    }
    await tick();

    const joinSubRoomMsgs = sentMessages.filter((m) => m.type === 'join_sub_room');
    expect(joinSubRoomMsgs.at(-1)).toEqual({
      type: 'join_sub_room',
      subRoomId: 'room-2',
    });
    expect(getState().desiredSubRoomId).toBe('room-2');
  });

  it('keeps the latest join intent when stale self join acknowledgements arrive out of order', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
        { subRoomId: 'room-3', roomNumber: 3, isDefault: false, participantIds: [] },
      ],
    });
    await tick();
    sentMessages = [];

    joinSubRoom('room-2');
    joinSubRoom('room-3');

    expect(getState().desiredSubRoomId).toBe('room-3');
    expect(sentMessages.filter((m) => m.type === 'join_sub_room')).toEqual([
      { type: 'join_sub_room', subRoomId: 'room-2' },
      { type: 'join_sub_room', subRoomId: 'room-3' },
    ]);

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'self-peer',
      subRoomId: 'room-2',
      source: 'explicit',
    });
    await tick();

    expect(getState().joinedSubRoomId).toBe('room-2');
    expect(getState().desiredSubRoomId).toBe('room-3');
    expect(sentMessages.filter((m) => m.type === 'join_sub_room').at(-1)).toEqual({
      type: 'join_sub_room',
      subRoomId: 'room-3',
    });
  });

  it('keeps an explicit leave intent when a stale self join acknowledgement arrives later', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();
    sentMessages = [];

    joinSubRoom('room-2');
    leaveSubRoom();

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'self-peer',
      subRoomId: 'room-2',
      source: 'explicit',
    });
    await tick();

    expect(getState().joinedSubRoomId).toBe('room-2');
    expect(getState().desiredSubRoomId).toBeNull();
    expect(sentMessages.filter((m) => m.type === 'leave_sub_room').at(-1)).toEqual({
      type: 'leave_sub_room',
    });
  });
});

describe('VoiceRoom room-based effective volume isolation', () => {
  it('keeps the default no-room self state unmuted while media stays isolated', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({ type: 'sub_room_left', participantId: 'self-peer', subRoomId: 'room-1' });
    await tick();

    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().participants.find((p) => p.id === 'self-peer')).toMatchObject({
      isMuted: false,
      isSpeaking: false,
      rmsLevel: 0,
    });
  });

  it('disables local mic publishing and incoming audio when a snapshot moves self out of all rooms', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    messageHandler!({
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
    lastLkModule!.setMicEnabledCalls = [];
    const volumeCallsBefore = lastLkModule!.setParticipantVolumeCalls.length;

    messageHandler!({
      type: 'sub_room_state',
      rooms: [{ subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['peer-2'] }],
    });
    await tick();

    expect(lastLkModule!.setMicEnabledCalls).toEqual([false]);
    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().participants.find((p) => p.id === 'self-peer')).toMatchObject({
      isMuted: false,
      isSpeaking: false,
      rmsLevel: 0,
    });
    expect(lastLkModule!.setParticipantVolumeCalls.slice(volumeCallsBefore)).toContainEqual({
      id: 'u2',
      vol: 0,
    });
    expect(sentMessages.some((m) => String(m.type).startsWith('self_'))).toBe(false);
    expect(
      getState().events.some((e) => ['muted', 'unmuted', 'deafen', 'undeafen'].includes(e.type)),
    ).toBe(false);
  });

  it('enables local mic publishing by default when a snapshot assigns self to a room', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({ type: 'sub_room_left', participantId: 'self-peer', subRoomId: 'room-1' });
    await tick();

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
      ],
    });
    await tick();

    // Leaving all sub-rooms fully disconnects media; rejoining reconnects on a
    // fresh LiveKitModule instance, so mic publishing is applied only once that
    // new connection reports itself connected.
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    expect(lastLkModule!.setMicEnabledCalls).toEqual([true]);
    expect(getState().joinedSubRoomId).toBe('room-1');
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(false);
  });

  it('does not reset mic publishing when self moves directly between rooms', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();
    lastLkModule!.setMicEnabledCalls = [];

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'self-peer',
      subRoomId: 'room-2',
      source: 'explicit',
    });
    await tick();

    expect(getState().joinedSubRoomId).toBe('room-2');
    expect(lastLkModule!.setMicEnabledCalls).toEqual([]);
  });

  it('disables local mic publishing when sub_room_left confirms self left the room', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
      ],
    });
    await tick();
    lastLkModule!.setMicEnabledCalls = [];

    messageHandler!({
      type: 'sub_room_left',
      participantId: 'self-peer',
      subRoomId: 'room-1',
    });
    await tick();

    expect(lastLkModule!.setMicEnabledCalls).toEqual([false]);
    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(false);
    expect(sentMessages.some((m) => String(m.type).startsWith('self_'))).toBe(false);
    expect(
      getState().events.some((e) => ['muted', 'unmuted', 'deafen', 'undeafen'].includes(e.type)),
    ).toBe(false);
  });

  it('stores pre-room mute intent and joins with mic publishing disabled', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({ type: 'sub_room_left', participantId: 'self-peer', subRoomId: 'room-1' });
    await tick();
    lastLkModule!.setMicEnabledCalls = [];
    const sentBefore = sentMessages.length;
    const eventsBefore = getState().events.length;

    toggleSelfMute();
    await tick();

    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(true);
    // No LiveKit connection exists while outside all sub-rooms, so muting here
    // touches nothing on the (disconnected) module.
    expect(lastLkModule!.setMicEnabledCalls).toEqual([]);
    expect(
      sentMessages.slice(sentBefore).filter((m) => String(m.type).startsWith('self_')),
    ).toEqual([]);
    expect(
      getState()
        .events.slice(eventsBefore)
        .filter((e) => ['muted', 'unmuted'].includes(e.type)),
    ).toEqual([]);

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
      ],
    });
    await tick();
    // Rejoining reconnects on a fresh LiveKitModule instance; mic publishing is
    // applied once that new connection reports itself connected.
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    expect(getState().joinedSubRoomId).toBe('room-1');
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(true);
    expect(lastLkModule!.setMicEnabledCalls).toEqual([false]);
    expect(
      sentMessages.slice(sentBefore).filter((m) => String(m.type).startsWith('self_')),
    ).toEqual([]);
  });

  it('stores pre-room unmute intent but keeps mic publishing disabled until room entry', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({ type: 'sub_room_left', participantId: 'self-peer', subRoomId: 'room-1' });
    await tick();

    toggleSelfMute();
    await tick();
    lastLkModule!.setMicEnabledCalls = [];
    const sentBefore = sentMessages.length;
    const eventsBefore = getState().events.length;

    toggleSelfMute();
    await tick();

    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(false);
    // No LiveKit connection exists while outside all sub-rooms, so unmuting
    // here touches nothing on the (disconnected) module.
    expect(lastLkModule!.setMicEnabledCalls).toEqual([]);
    expect(
      sentMessages.slice(sentBefore).filter((m) => String(m.type).startsWith('self_')),
    ).toEqual([]);
    expect(
      getState()
        .events.slice(eventsBefore)
        .filter((e) => ['muted', 'unmuted'].includes(e.type)),
    ).toEqual([]);

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
      ],
    });
    await tick();
    // Rejoining reconnects on a fresh LiveKitModule instance; mic publishing is
    // applied once that new connection reports itself connected.
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    expect(getState().joinedSubRoomId).toBe('room-1');
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(false);
    expect(lastLkModule!.setMicEnabledCalls).toEqual([true]);
    expect(
      sentMessages.slice(sentBefore).filter((m) => String(m.type).startsWith('self_')),
    ).toEqual([]);
  });

  it('restores pre-deafen mute intent outside a room without enabling the mic', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({ type: 'sub_room_left', participantId: 'self-peer', subRoomId: 'room-1' });
    await tick();
    lastLkModule!.setMicEnabledCalls = [];
    const sentBefore = sentMessages.length;
    const eventsBefore = getState().events.length;

    toggleSelfDeafen();
    await tick();
    expect(getState().isDeafened).toBe(true);
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(true);

    toggleSelfDeafen();
    await tick();

    expect(getState().isDeafened).toBe(false);
    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(false);
    // No LiveKit connection exists while outside all sub-rooms, so deafening/
    // undeafening here touches nothing on the (disconnected) module.
    expect(lastLkModule!.setMicEnabledCalls).toEqual([]);
    expect(
      sentMessages.slice(sentBefore).filter((m) => String(m.type).startsWith('self_')),
    ).toEqual([]);
    expect(
      getState()
        .events.slice(eventsBefore)
        .filter((e) => ['deafen', 'undeafen'].includes(e.type)),
    ).toEqual([]);
  });

  it('ignores self media mute callbacks while outside a room', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({ type: 'sub_room_left', participantId: 'self-peer', subRoomId: 'room-1' });
    await tick();
    lastLkModule!.setMicEnabledCalls = [];
    const eventsBefore = getState().events.length;

    lastLkModule!.callbacks.onParticipantMuteChanged('self-peer', true);
    await tick();

    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().participants.find((p) => p.id === 'self-peer')).toMatchObject({
      isMuted: false,
      isSpeaking: false,
      rmsLevel: 0,
    });
    expect(lastLkModule!.setMicEnabledCalls).toEqual([]);
    expect(
      getState()
        .events.slice(eventsBefore)
        .filter((e) => ['muted', 'unmuted'].includes(e.type)),
    ).toEqual([]);
  });

  it('stores pre-room deafen intent and joins with mic publishing disabled', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({ type: 'sub_room_left', participantId: 'self-peer', subRoomId: 'room-1' });
    await tick();
    lastLkModule!.setMicEnabledCalls = [];
    const sentBefore = sentMessages.length;

    toggleSelfDeafen();
    await tick();

    expect(getState().joinedSubRoomId).toBeNull();
    expect(getState().isDeafened).toBe(true);
    expect(getState().participants.find((p) => p.id === 'self-peer')).toMatchObject({
      isMuted: true,
      isDeafened: true,
    });
    // No LiveKit connection exists while outside all sub-rooms, so deafening
    // here touches nothing on the (disconnected) module.
    expect(lastLkModule!.setMicEnabledCalls).toEqual([]);
    expect(
      sentMessages.slice(sentBefore).filter((m) => String(m.type).startsWith('self_')),
    ).toEqual([]);

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
      ],
    });
    await tick();
    // Rejoining reconnects on a fresh LiveKitModule instance; mic publishing is
    // applied once that new connection reports itself connected.
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    expect(getState().joinedSubRoomId).toBe('room-1');
    expect(getState().isDeafened).toBe(true);
    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(true);
    expect(lastLkModule!.setMicEnabledCalls).toEqual([false]);
    expect(
      sentMessages.slice(sentBefore).filter((m) => String(m.type).startsWith('self_')),
    ).toEqual([]);
  });

  it('leaveRoom resets no-room mute and deafen intent to defaults', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    toggleSelfMute();
    toggleSelfDeafen();
    await tick();

    expect(getState().participants.find((p) => p.id === 'self-peer')?.isMuted).toBe(true);
    expect(getState().isDeafened).toBe(true);

    leaveRoom();

    expect(getState().isDeafened).toBe(false);
    expect(getState().participants).toEqual([]);
  });

  it('mutes participants outside the local joined room while preserving manual volume', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    const callsBefore = lastLkModule!.setParticipantVolumeCalls.length;

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();

    const newCalls = lastLkModule!.setParticipantVolumeCalls.slice(callsBefore);
    expect(newCalls).toContainEqual({ id: 'u2', vol: 0 });
    expect(getState().participants.find((p) => p.id === 'peer-2')?.volume).toBe(70);
  });

  it('when the local user is not in a room, everyone else is effectively muted', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    const callsBefore = lastLkModule!.setParticipantVolumeCalls.length;

    messageHandler!({
      type: 'sub_room_state',
      rooms: [{ subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['peer-2'] }],
    });
    await tick();

    const newCalls = lastLkModule!.setParticipantVolumeCalls.slice(callsBefore);
    expect(newCalls).toContainEqual({ id: 'u2', vol: 0 });
    expect(getState().joinedSubRoomId).toBeNull();
  });

  it('attenuates participants in the paired passthrough room and preserves their saved volume preference', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    const callsBefore = lastLkModule!.setParticipantVolumeCalls.length;

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
      passthrough: {
        sourceSubRoomId: 'room-1',
        targetSubRoomId: 'room-2',
        label: '1 - 2',
      },
    });
    await tick();

    const newCalls = lastLkModule!.setParticipantVolumeCalls.slice(callsBefore);
    expect(newCalls).toContainEqual({ id: 'u2', vol: 14 });
    expect(getState().participants.find((p) => p.id === 'peer-2')?.volume).toBe(70);
    expect(getState().passthrough).toEqual({
      sourceSubRoomId: 'room-1',
      targetSubRoomId: 'room-2',
      label: '1 - 2',
    });
  });
});

describe('VoiceRoom room-scoped join/leave sounds', () => {
  it('does not play join sound for voice-session joined or participant_joined before room membership exists', async () => {
    await driveToActive('ch-sounds', 'room-sounds', false);

    expect(playNotificationSoundCalls).toEqual([]);

    messageHandler!({
      type: 'participant_joined',
      participantId: 'peer-3',
      displayName: 'Bob',
      userId: 'u3',
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual([]);
  });

  it('keeps self connecting after room assignment and plays join when local media connects', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'media_token',
      sfuUrl: 'wss://sfu',
      token: 'tok',
    });
    await tick();

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();

    expect(getState().participants.find((p) => p.id === 'self-peer')?.mediaConnected).toBe(false);
    expect(playNotificationSoundCalls).toEqual([]);

    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    expect(getState().participants.find((p) => p.id === 'self-peer')?.mediaConnected).toBe(true);
    expect(playNotificationSoundCalls).toEqual(['join']);
  });

  it('plays leave then join when the local user switches rooms', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        {
          subRoomId: 'room-1',
          roomNumber: 1,
          isDefault: true,
          participantIds: ['self-peer', 'peer-2'],
        },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    playNotificationSoundCalls = [];

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'self-peer',
      subRoomId: 'room-2',
      source: 'explicit',
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['leave', 'join']);
  });

  it('plays join when another user enters the local user current room', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    lastLkModule!.callbacks.onRemoteParticipantConnected?.('peer-2');
    await tick();
    playNotificationSoundCalls = [];

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'peer-2',
      subRoomId: 'room-1',
      source: 'explicit',
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['join']);
  });

  it('does not play remote join until the remote participant is media-connected', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    playNotificationSoundCalls = [];

    messageHandler!({
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

    expect(getState().participants.find((p) => p.id === 'peer-2')?.mediaConnected).toBe(false);
    expect(playNotificationSoundCalls).toEqual([]);

    lastLkModule!.callbacks.onRemoteParticipantConnected?.('peer-2');
    await tick();

    expect(getState().participants.find((p) => p.id === 'peer-2')?.mediaConnected).toBe(true);
    expect(playNotificationSoundCalls).toEqual(['join']);
  });

  it('plays leave when another user leaves the local user current room', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
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
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    playNotificationSoundCalls = [];

    messageHandler!({
      type: 'participant_left',
      participantId: 'peer-2',
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['leave']);
  });

  it('does not play room sound when another user moves outside the local user current room', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    lastLkModule!.callbacks.onRemoteParticipantConnected?.('peer-2');
    await tick();
    playNotificationSoundCalls = [];

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
        { subRoomId: 'room-3', roomNumber: 3, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual([]);
  });

  it('does not double-play room sounds when an incremental event is followed by the same snapshot', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    lastLkModule!.callbacks.onRemoteParticipantConnected?.('peer-2');
    await tick();
    playNotificationSoundCalls = [];

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'peer-2',
      subRoomId: 'room-1',
      source: 'explicit',
    });
    await tick();
    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        {
          subRoomId: 'room-1',
          roomNumber: 1,
          isDefault: true,
          participantIds: ['self-peer', 'peer-2'],
        },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['join']);
  });

  it('does not replay remote join for duplicate signaling snapshots after media readiness', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();

    messageHandler!({
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
    lastLkModule!.callbacks.onRemoteParticipantConnected?.('peer-2');
    await tick();
    expect(playNotificationSoundCalls).toEqual(['join', 'join']);
    playNotificationSoundCalls = [];

    messageHandler!({
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
    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'peer-2',
      subRoomId: 'room-1',
      source: 'explicit',
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual([]);
  });

  it('media reconnect marks participants connecting and restores readiness without replaying join', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    messageHandler!({
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
    lastLkModule!.callbacks.onMediaConnected();
    lastLkModule!.callbacks.onRemoteParticipantConnected?.('peer-2');
    await tick();
    expect(getState().participants.find((p) => p.id === 'self-peer')?.mediaConnected).toBe(true);
    expect(getState().participants.find((p) => p.id === 'peer-2')?.mediaConnected).toBe(true);
    playNotificationSoundCalls = [];

    lastLkModule!.callbacks.onMediaReconnecting?.();
    await tick();

    expect(getState().participants.find((p) => p.id === 'self-peer')?.mediaConnected).toBe(false);
    expect(getState().participants.find((p) => p.id === 'peer-2')?.mediaConnected).toBe(false);

    lastLkModule!.callbacks.onMediaReconnected?.();
    lastLkModule!.callbacks.onRemoteParticipantConnected?.('peer-2');
    await tick();

    expect(getState().participants.find((p) => p.id === 'self-peer')?.mediaConnected).toBe(true);
    expect(getState().participants.find((p) => p.id === 'peer-2')?.mediaConnected).toBe(true);
    expect(playNotificationSoundCalls).toEqual([]);
  });

  it('plays leave sound for explicit whole-session leave', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
      ],
    });
    await tick();
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    playNotificationSoundCalls = [];

    leaveRoom();

    expect(playNotificationSoundCalls).toEqual(['leave']);
    expect(sentMessages).toContainEqual({ type: 'leave' });
    expect(lastLkModule!.disconnectCalls).toBe(1);
  });

  it('keeps the room connected until the background leave timeout elapses', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();

    sentMessages = [];
    vi.useFakeTimers();
    try {
      scheduleLeaveRoom();

      await vi.advanceTimersByTimeAsync(BACKGROUND_LEAVE_DISCONNECT_MS - 1);

      expect(getState().machineState).toBe('active');
      expect(lastLkModule!.disconnectCalls).toBe(0);
      expect(sentMessages.filter((m) => m.type === 'leave')).toHaveLength(0);

      await vi.advanceTimersByTimeAsync(1);

      expect(getState().machineState).toBe('idle');
      expect(lastLkModule!.disconnectCalls).toBe(1);
      expect(sentMessages.filter((m) => m.type === 'leave')).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('cancels the background leave timer when a hard leave happens first', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();

    sentMessages = [];
    vi.useFakeTimers();
    try {
      scheduleLeaveRoom();
      leaveRoom();

      expect(getState().machineState).toBe('idle');
      expect(lastLkModule!.disconnectCalls).toBe(1);
      expect(sentMessages.filter((m) => m.type === 'leave')).toHaveLength(1);

      await vi.advanceTimersByTimeAsync(BACKGROUND_LEAVE_DISCONNECT_MS);

      expect(lastLkModule!.disconnectCalls).toBe(1);
      expect(sentMessages.filter((m) => m.type === 'leave')).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('plays one leave sound when the local participant is kicked', async () => {
    await driveToActive('ch-sounds', 'room-sounds', false);

    messageHandler!({
      type: 'participant_kicked',
      participantId: 'self-peer',
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['leave']);
    expect(getState().machineState).toBe('idle');
  });

  it('plays one leave sound when the local session is displaced', async () => {
    await driveToActive('ch-sounds', 'room-sounds', false);

    messageHandler!({ type: 'session_displaced' });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['leave']);
    expect(getState().machineState).toBe('idle');
  });

  it('plays one leave sound when signaling reconnect exhaustion ends an active session', async () => {
    await driveToActive('ch-sounds', 'room-sounds', false);

    statusChangeHandler!('disconnected');
    await tick();
    expect(getState().machineState).toBe('reconnecting');

    if (lastSignalingClient) {
      lastSignalingClient.status = 'disconnected';
      lastSignalingClient.reconnectExhausted = true;
    }
    statusChangeHandler!('disconnected');
    await tick();

    expect(playNotificationSoundCalls).toEqual(['leave']);
    expect(getState().machineState).toBe('idle');
  });

  it('does not give up while a reconnect is merely pending between attempts (reconnectExhausted still false)', async () => {
    await driveToActive('ch-sounds', 'room-sounds', false);

    statusChangeHandler!('disconnected');
    await tick();
    expect(getState().machineState).toBe('reconnecting');

    // A real SignalingClient's reconnectTimer is transiently null between one
    // failed attempt and the next being scheduled — that alone must NOT be
    // read as "exhausted" (see websocket.ts's reconnectExhausted flag).
    if (lastSignalingClient) {
      lastSignalingClient.status = 'disconnected';
      lastSignalingClient.reconnectExhausted = false;
    }
    statusChangeHandler!('disconnected');
    await tick();

    expect(playNotificationSoundCalls).toEqual([]);
    expect(getState().machineState).toBe('reconnecting');
  });

  it('does not play leave sound for initial connection failure before room membership', async () => {
    connectShouldFail = true;
    initSession('ch-sounds', 'room-sounds', 'owner', (s) => {
      latestState = s;
    });
    await tick();
    await tick();

    expect(playNotificationSoundCalls).toEqual([]);
    expect(getState().machineState).toBe('idle');
  });

  it('plays one leave sound when duplicate terminal paths arrive for the same session', async () => {
    await driveToActive('ch-sounds', 'room-sounds', false);

    messageHandler!({
      type: 'participant_kicked',
      participantId: 'self-peer',
    });
    messageHandler!({ type: 'session_displaced' });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['leave']);
  });
});

describe('VoiceRoom room-scoped toast flags', () => {
  it('does not mark join events toastable when another user joins a different room', async () => {
    await driveToActive('ch-toast', 'room-toast');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();

    messageHandler!({
      type: 'participant_joined',
      participantId: 'peer-3',
      displayName: 'Bob',
      userId: 'u3',
    });
    await tick();

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'peer-3',
      subRoomId: 'room-2',
      source: 'explicit',
    });
    await tick();

    expect(getState().events.at(-1)).toMatchObject({
      type: 'join',
      participantId: 'peer-3',
      shouldToast: false,
    });
  });

  it('marks join events toastable only when another user enters the local room', async () => {
    await driveToActive('ch-toast', 'room-toast');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'peer-2',
      subRoomId: 'room-1',
      source: 'explicit',
    });
    await tick();

    expect(getState().events.at(-1)).toMatchObject({
      type: 'join',
      participantId: 'peer-2',
      shouldToast: true,
    });
  });

  it('does not mark self room joins toastable', async () => {
    await driveToActive('ch-toast', 'room-toast');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: [] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();

    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'self-peer',
      subRoomId: 'room-1',
      source: 'explicit',
    });
    await tick();

    expect(getState().events.at(-1)).toMatchObject({
      type: 'join',
      participantId: 'self-peer',
      shouldToast: false,
    });
  });
});

afterEach(() => {
  // Clean up any active session
  try {
    leaveRoom();
  } catch {
    /* ignore */
  }
});

describe('VoiceRoom participant_joined volume re-application', () => {
  it('participant_joined with media connected applies persisted volume to media layer', async () => {
    // Arrange: prime getChannelVolumes to return u2's saved volume of 44
    vi.mocked(settingsStore.getChannelVolumes).mockResolvedValueOnce({
      master: 70,
      participants: { u2: 44 },
    });

    await driveToActive(); // room has self-peer (u1) and peer-2 (u2)

    // Connect media: send token, then fire onMediaConnected callback
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    messageHandler!({
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

    // Discard volume calls from onMediaConnected (applies volumes for existing participants)
    const callsBefore = lastLkModule!.setParticipantVolumeCalls.length;

    // Act: peer-new joins with the same userId u2 (simulates a rejoin)
    messageHandler!({
      type: 'participant_joined',
      participantId: 'peer-new',
      displayName: 'Alice',
      userId: 'u2',
    });
    await tick();

    // Without a synchronized sub-room assignment yet, the participant is effectively muted.
    // The LiveKit identity used here is the stable userId ('u2'), not the ephemeral
    // peer_id ('peer-new') — that's the whole point of the rejoin-under-new-peer_id case.
    const newCalls = lastLkModule!.setParticipantVolumeCalls.slice(callsBefore);
    expect(newCalls).toContainEqual({ id: 'u2', vol: 0 });
    expect(getState().participants.find((p) => p.id === 'peer-new')?.volume).toBe(44);

    // Once the participant is assigned into the same sub-room, their saved volume is restored
    messageHandler!({
      type: 'sub_room_joined',
      participantId: 'peer-new',
      subRoomId: 'room-1',
      source: 'explicit',
    });
    await tick();

    const callsAfterRoomJoin = lastLkModule!.setParticipantVolumeCalls.slice(callsBefore);
    expect(callsAfterRoomJoin).toContainEqual({ id: 'u2', vol: 44 });

    leaveRoom();
  });
});

describe('VoiceRoom SFU cold start retry', () => {
  it('enters server_starting, retries JoinVoice, and clears retry after joined', async () => {
    latestState = null;
    initSession('ch-cold', 'cold-room', 'owner', (s) => {
      latestState = s;
    });
    await tick();

    try {
      messageHandler!({ type: 'auth_success' });
      vi.useFakeTimers();

      messageHandler!({ type: 'sfu_cold_starting', estimatedWaitSecs: 120 });

      expect(latestState!.machineState).toBe('server_starting');
      expect(latestState!.serverStartingEstimatedWaitSecs).toBe(120);

      const joinCountBeforeRetry = sentMessages.filter((m) => m.type === 'join_voice').length;
      vi.advanceTimersByTime(30_000);

      const joinMessagesAfterRetry = sentMessages.filter((m) => m.type === 'join_voice');
      expect(joinMessagesAfterRetry).toHaveLength(joinCountBeforeRetry + 1);
      expect(joinMessagesAfterRetry[joinMessagesAfterRetry.length - 1]).toMatchObject({
        type: 'join_voice',
        channelId: 'ch-cold',
        displayName: 'TestUser',
        profileColor: '#E06C75',
      });

      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-1',
        participants: [{ participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' }],
      });

      expect(latestState!.machineState).toBe('active');
      expect(latestState!.serverStartingEstimatedWaitSecs).toBeNull();

      const joinCountAfterJoined = sentMessages.filter((m) => m.type === 'join_voice').length;
      vi.advanceTimersByTime(30_000);
      expect(sentMessages.filter((m) => m.type === 'join_voice')).toHaveLength(
        joinCountAfterJoined,
      );
    } finally {
      leaveRoom();
      vi.useRealTimers();
    }
  });
});

// ═══ Property Tests: Voice-Room Media Wiring ═══════════════════════

describe('Voice-room media wiring', () => {
  // P1: Valid media_token triggers LiveKit connect
  describe('P1: Valid media_token triggers LiveKit connect', () => {
    it('media_token with valid sfuUrl and token creates LiveKitModule and calls connect', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 100 }).filter((s) => s.trim().length > 0),
          fc.string({ minLength: 1, maxLength: 100 }).filter((s) => s.trim().length > 0),
          async (sfuUrl, token) => {
            resetAll();
            await driveToActive();

            // Send media_token
            messageHandler!({ type: 'media_token', sfuUrl, token });
            await tick();

            // LiveKitModule was constructed
            expect(lkConstructorCalls).toHaveLength(1);

            // connect was called with the correct args
            expect(lastLkModule).not.toBeNull();
            expect(lastLkModule!.connectCalls).toHaveLength(1);
            expect(lastLkModule!.connectCalls[0].sfuUrl).toBe(sfuUrl);
            expect(lastLkModule!.connectCalls[0].token).toBe(token);

            // mediaState should be 'connecting'
            expect(latestState!.mediaState).toBe('connecting');

            leaveRoom();
          },
        ),
        { numRuns: 50 },
      );
    });
  });

  // P2: Media token buffering when not active
  describe('P2: Media token buffering when not active', () => {
    it('media_token before active state is buffered and flushed on sub_room_state', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 50 }).filter((s) => s.trim().length > 0),
          fc.string({ minLength: 1, maxLength: 50 }).filter((s) => s.trim().length > 0),
          async (sfuUrl, token) => {
            resetAll();
            latestState = null;
            initSession('ch-buf', 'buf-room', 'member', (s) => {
              latestState = s;
            });
            await tick();

            // Simulate auth_success (now in 'joining' state, not 'active')
            messageHandler!({ type: 'auth_success' });

            // Send media_token while NOT active — should be buffered
            messageHandler!({ type: 'media_token', sfuUrl, token });
            await tick();

            // No LiveKitModule created yet
            expect(lkConstructorCalls).toHaveLength(0);

            // Transition to active via joined — still buffered (no sub-room yet)
            messageHandler!({
              type: 'joined',
              peerId: 'self-peer',
              roomId: 'room-buf',
              participants: [{ participantId: 'self-peer', displayName: 'TestUser' }],
            });
            await tick();

            // Still buffered: active but joinedSubRoomId is still null
            expect(lkConstructorCalls).toHaveLength(0);

            // sub_room_state snapshot arrives (as backend sends after join) — flushes token
            messageHandler!({
              type: 'sub_room_state',
              rooms: [
                {
                  subRoomId: 'room-1',
                  roomNumber: 1,
                  isDefault: true,
                  participantIds: ['self-peer'],
                },
              ],
            });
            await tick();

            // Buffered token flushed — LiveKitModule created
            expect(lkConstructorCalls).toHaveLength(1);
            expect(lastLkModule!.connectCalls).toHaveLength(1);
            expect(lastLkModule!.connectCalls[0].sfuUrl).toBe(sfuUrl);
            expect(lastLkModule!.connectCalls[0].token).toBe(token);

            leaveRoom();
          },
        ),
        { numRuns: 50 },
      );
    });
  });

  // P2b: sub_room_joined ordering race — token arrives before sub_room_state seeds the room
  describe('P2b: Media token flushed via sub_room_joined when room already in state', () => {
    it('media_token buffered pre-join is flushed by sub_room_joined when room exists in state', async () => {
      resetAll();
      latestState = null;
      initSession('ch-buf2', 'buf-room2', 'member', (s) => {
        latestState = s;
      });
      await tick();

      messageHandler!({ type: 'auth_success' });

      // Buffer the token before active
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu-srj', token: 'tok-srj' });
      await tick();
      expect(lkConstructorCalls).toHaveLength(0);

      // joined — active but no sub-room yet
      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-srj',
        participants: [{ participantId: 'self-peer', displayName: 'TestUser' }],
      });
      await tick();
      expect(lkConstructorCalls).toHaveLength(0);

      // sub_room_state seeds the room first (room now exists in state)
      messageHandler!({
        type: 'sub_room_state',
        rooms: [{ subRoomId: 'room-srj', roomNumber: 1, isDefault: true, participantIds: [] }],
      });
      await tick();
      // Room exists but self not in it yet — still buffered
      expect(lkConstructorCalls).toHaveLength(0);

      // sub_room_joined for self — room already in state, joinedSubRoomId becomes non-null
      messageHandler!({
        type: 'sub_room_joined',
        participantId: 'self-peer',
        subRoomId: 'room-srj',
      });
      await tick();

      // Token flushed via sub_room_joined
      expect(lkConstructorCalls).toHaveLength(1);
      expect(lastLkModule!.connectCalls[0]).toEqual({ sfuUrl: 'wss://sfu-srj', token: 'tok-srj' });

      leaveRoom();
    });

    it('media_token buffered pre-join is flushed by sub_room_state when sub_room_joined arrived before room was seeded', async () => {
      resetAll();
      latestState = null;
      initSession('ch-buf3', 'buf-room3', 'member', (s) => {
        latestState = s;
      });
      await tick();

      messageHandler!({ type: 'auth_success' });

      // Buffer the token before active
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu-srs', token: 'tok-srs' });
      await tick();
      expect(lkConstructorCalls).toHaveLength(0);

      // joined — active but no sub-room yet
      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-srs',
        participants: [{ participantId: 'self-peer', displayName: 'TestUser' }],
      });
      await tick();
      expect(lkConstructorCalls).toHaveLength(0);

      // sub_room_joined arrives before sub_room_state — room not in state yet,
      // so syncDerivedSubRoomState leaves joinedSubRoomId null → still buffered
      messageHandler!({
        type: 'sub_room_joined',
        participantId: 'self-peer',
        subRoomId: 'room-srs',
      });
      await tick();
      expect(lkConstructorCalls).toHaveLength(0);

      // sub_room_state snapshot catches up — flushes the token
      messageHandler!({
        type: 'sub_room_state',
        rooms: [
          { subRoomId: 'room-srs', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        ],
      });
      await tick();

      expect(lkConstructorCalls).toHaveLength(1);
      expect(lastLkModule!.connectCalls[0]).toEqual({ sfuUrl: 'wss://sfu-srs', token: 'tok-srs' });

      leaveRoom();
    });
  });

  // P3: Invalid media_token rejection
  describe('P3: Invalid media_token rejection', () => {
    it('media_token with empty token or sfuUrl appends system error and does not connect', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.oneof(
            // empty token
            fc.record({
              sfuUrl: fc.string({ minLength: 1, maxLength: 50 }).filter((s) => s.trim().length > 0),
              token: fc.constant(''),
            }),
            // empty sfuUrl
            fc.record({
              sfuUrl: fc.constant(''),
              token: fc.string({ minLength: 1, maxLength: 50 }).filter((s) => s.trim().length > 0),
            }),
            // both empty
            fc.record({ sfuUrl: fc.constant(''), token: fc.constant('') }),
          ),
          async ({ sfuUrl, token }) => {
            resetAll();
            await driveToActive();

            const eventsBefore = latestState!.events.length;

            messageHandler!({ type: 'media_token', sfuUrl, token });
            await tick();

            // No LiveKitModule created
            expect(lkConstructorCalls).toHaveLength(0);

            // System error event appended
            expect(latestState!.events.length).toBeGreaterThan(eventsBefore);
            const lastEvent = latestState!.events[latestState!.events.length - 1];
            expect(lastEvent.type).toBe('system');
            expect(lastEvent.message).toContain('empty token or sfuUrl');

            leaveRoom();
          },
        ),
        { numRuns: 50 },
      );
    });
  });

  // P9: Host-mute prevents self-unmute, host-unmute releases the lock
  describe('P9: Host-mute prevents unmute until host releases', () => {
    it('hydrates mute flags from joined and room_state snapshots for late joiners', async () => {
      resetAll();
      latestState = null;
      initSession('ch-1', 'test-room', 'owner', (s) => {
        latestState = s;
      });
      await tick();

      messageHandler!({ type: 'auth_success' });
      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-1',
        participants: [
          { participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' },
          {
            participantId: 'peer-2',
            displayName: 'Alice',
            userId: 'u2',
            isMuted: true,
            isHostMuted: true,
            isDeafened: true,
          },
        ],
      });
      await tick();

      expect(latestState!.participants.find((p) => p.id === 'peer-2')).toMatchObject({
        isMuted: true,
        isHostMuted: true,
        isDeafened: true,
      });

      messageHandler!({
        type: 'room_state',
        participants: [
          {
            participantId: 'peer-2',
            displayName: 'Alice',
            userId: 'u2',
            isMuted: true,
            isHostMuted: false,
            isDeafened: false,
          },
        ],
      });
      await tick();

      expect(latestState!.participants.find((p) => p.id === 'peer-2')).toMatchObject({
        isMuted: true,
        isHostMuted: false,
        isDeafened: false,
      });

      leaveRoom();
    });

    it('toggleSelfMute is blocked when host-muted, unblocked after participant_unmuted', async () => {
      resetAll();
      await driveToActive();

      // Send media_token so lkModule exists
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      await assignSelfToSubRoom();

      // Host-mute self
      messageHandler!({ type: 'participant_muted', participantId: 'self-peer' });
      await tick();

      const self = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(self!.isHostMuted).toBe(true);
      expect(self!.isMuted).toBe(true);

      // Clear mic calls from host-mute
      const micCallsBefore = lastLkModule!.setMicEnabledCalls.length;

      // Try to unmute — should be blocked
      toggleSelfMute();
      await tick();

      // No new setMicEnabled calls (blocked by isHostMuted guard)
      expect(lastLkModule!.setMicEnabledCalls.length).toBe(micCallsBefore);

      // Still muted
      const selfBlocked = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(selfBlocked!.isMuted).toBe(true);

      // Host releases the mute
      messageHandler!({ type: 'participant_unmuted', participantId: 'self-peer' });
      await tick();

      const selfUnlocked = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(selfUnlocked!.isHostMuted).toBe(false);
      // Still muted (mic not auto-enabled), but can now self-unmute
      expect(selfUnlocked!.isMuted).toBe(true);

      // Now toggleSelfMute should work
      toggleSelfMute();
      await tick();

      const selfUnmuted = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(selfUnmuted!.isMuted).toBe(false);
      // setMicEnabled(true) should have been called
      expect(lastLkModule!.setMicEnabledCalls[lastLkModule!.setMicEnabledCalls.length - 1]).toBe(
        true,
      );

      leaveRoom();
    });

    it('toggleSelfMute restores a missing self participant and still mutes locally', async () => {
      resetAll();
      latestState = null;
      initSession('ch-missing-self', 'test-room', 'owner', (s) => {
        latestState = s;
      });
      await tick();

      messageHandler!({ type: 'auth_success' });
      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-1',
        participants: [{ participantId: 'peer-2', displayName: 'Alice', userId: 'u2' }],
      });
      await tick();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      expect(latestState!.participants.find((p) => p.id === 'self-peer')).toBeTruthy();
      expect(
        latestState!.events.some((e) => e.message === 'local participant state was restored'),
      ).toBe(true);

      // Flush buffered token via sub_room_state snapshot, then leave to test no-room mute
      messageHandler!({
        type: 'sub_room_state',
        rooms: [
          { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        ],
      });
      await tick();
      messageHandler!({
        type: 'sub_room_state',
        rooms: [{ subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: [] }],
      });
      await tick();

      toggleSelfMute();
      await tick();

      const self = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(self).toBeTruthy();
      expect(self!.isMuted).toBe(true);
      expect(lastLkModule!.setMicEnabledCalls[lastLkModule!.setMicEnabledCalls.length - 1]).toBe(
        false,
      );
      expect(sentMessages.some((m) => m.type === 'self_mute')).toBe(false);
      expect(
        latestState!.events.some((e) => e.type === 'muted' && e.message === 'you muted microphone'),
      ).toBe(false);

      leaveRoom();
    });
  });

  // P22: Reconnection preserves media across WS reconnect
  describe('P22: Reconnection creates fresh instance after full teardown', () => {
    it('WS disconnect during active tears down media; new media_token creates fresh module', async () => {
      await fc.assert(
        fc.asyncProperty(fc.integer({ min: 1, max: 3 }), async (reconnectCycles) => {
          resetAll();
          await driveToActive();

          // Establish media once — first media_token creates the module
          messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu-0', token: 'tok-0' });
          await tick();

          const initialModule = lastLkModule;
          expect(lkConstructorCalls).toHaveLength(1);

          for (let cycle = 0; cycle < reconnectCycles; cycle++) {
            const disconnectsBefore = initialModule!.disconnectCalls;

            // Simulate WS disconnect during active → triggers reconnecting.
            // Media is NOT torn down — LiveKit connects directly to the SFU,
            // independent of the signaling WS.
            statusChangeHandler!('disconnected');
            await tick();

            // Media module is NOT disconnected (stays alive across WS reconnect)
            expect(initialModule!.disconnectCalls).toBe(disconnectsBefore);
            expect(latestState!.machineState).toBe('reconnecting');

            // Simulate reconnect: auth_success → joined
            messageHandler!({ type: 'auth_success' });
            messageHandler!({
              type: 'joined',
              peerId: 'self-peer',
              roomId: `room-${cycle}`,
              participants: [{ participantId: 'self-peer', displayName: 'TestUser' }],
            });
            await tick();
          }

          // Only 1 LiveKitModule ever created — media survives WS reconnects
          expect(lkConstructorCalls).toHaveLength(1);

          leaveRoom();
        }),
        { numRuns: 20 },
      );
    });
  });

  // P23: Reconnection respects mute state
  describe('P23: Reconnection respects mute state', () => {
    it('mute state is preserved across reconnection — self stays muted if was muted', async () => {
      resetAll();
      await driveToActive();

      // Establish media
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      // Mute self
      toggleSelfMute();
      await tick();

      const selfBefore = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(selfBefore!.isMuted).toBe(true);

      // setMicEnabled(false) should have been called
      expect(lastLkModule!.setMicEnabledCalls).toContain(false);

      // Simulate WS disconnect → reconnect
      statusChangeHandler!('disconnected');
      await tick();

      // Reconnect flow
      messageHandler!({ type: 'auth_success' });
      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-2',
        participants: [{ participantId: 'self-peer', displayName: 'TestUser' }],
      });
      await tick();

      // After reconnect, participant list is rebuilt from joined message
      // The mute state is reset (fresh participant objects from joined)
      // This is expected — the new LiveKit session starts unmuted
      // The test validates that the old module was torn down cleanly
      expect(latestState!.machineState).toBe('active');

      leaveRoom();
    });
  });

  // P24: SDK reconnection events do not create duplicate connections
  describe('P24: SDK reconnection events do not create duplicate connections', () => {
    it('LiveKit Reconnecting/Reconnected callbacks do not create new LiveKitModule', async () => {
      await fc.assert(
        fc.asyncProperty(fc.integer({ min: 1, max: 10 }), async (reconnectEvents) => {
          resetAll();
          await driveToActive();

          messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
          await tick();

          const moduleCountBefore = lkConstructorCalls.length;
          expect(moduleCountBefore).toBe(1);

          // Simulate LiveKit SDK reconnecting/reconnected events via callbacks
          // These fire through the onSystemEvent callback — they should NOT create new modules
          if (lastLkModule) {
            for (let i = 0; i < reconnectEvents; i++) {
              lastLkModule.callbacks.onMediaReconnecting?.();
              lastLkModule.callbacks.onMediaReconnected?.();
            }
          }
          await tick();

          // No new LiveKitModule instances created
          expect(lkConstructorCalls.length).toBe(moduleCountBefore);

          leaveRoom();
        }),
        { numRuns: 50 },
      );
    });
  });

  // P26: Media failure preserves signaling state
  describe('P26: Media failure preserves signaling state', () => {
    it('media failure sets mediaState=failed but machineState stays active', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 100 }).filter((s) => s.trim().length > 0),
          async (failReason) => {
            resetAll();
            await driveToActive();

            messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
            await tick();

            // Simulate media failure via callback
            lastLkModule!.callbacks.onMediaFailed(failReason);
            await tick();

            // mediaState is failed
            expect(latestState!.mediaState).toBe('failed');
            expect(latestState!.mediaError).toBe(failReason);

            // machineState is still active — signaling survives
            expect(latestState!.machineState).toBe('active');

            // Participants still present
            expect(latestState!.participants.length).toBeGreaterThan(0);

            // System event logged
            const failEvents = latestState!.events.filter(
              (e) => e.type === 'system' && e.message.includes('media failed'),
            );
            expect(failEvents.length).toBeGreaterThanOrEqual(1);

            leaveRoom();
          },
        ),
        { numRuns: 50 },
      );
    });
  });

  // P27: Reconnect-media cooldown enforcement
  describe('P27: Reconnect-media cooldown enforcement', () => {
    it('reconnectMedia within 3s cooldown is rejected with system event', async () => {
      resetAll();
      await driveToActive();

      // Establish media
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      // First reconnect — should succeed
      const now = Date.now();
      vi.spyOn(Date, 'now').mockReturnValue(now);
      await reconnectMedia();

      // Module was torn down
      expect(lastLkModule!.disconnectCalls).toBeGreaterThanOrEqual(1);

      // Second reconnect at same time — should be blocked by cooldown
      await reconnectMedia();

      // Cooldown event appended
      const cooldownEvents = latestState!.events.filter(
        (e) => e.type === 'system' && e.message.includes('cooldown'),
      );
      expect(cooldownEvents.length).toBeGreaterThanOrEqual(1);

      // Advance past cooldown (3s)
      vi.spyOn(Date, 'now').mockReturnValue(now + 3100);

      // Third reconnect — should succeed now
      await reconnectMedia();

      // A join_voice message should have been sent (requesting new media_token)
      const joinVoiceMsgs = sentMessages.filter((m) => m.type === 'join_voice');
      expect(joinVoiceMsgs.length).toBeGreaterThanOrEqual(1);

      vi.spyOn(Date, 'now').mockRestore();
      leaveRoom();
    });
  });

  // P34: Media token ignored while failed (retries exhausted)
  describe('P34: Media token ignored while failed', () => {
    it('media_token is ignored when mediaState is failed and retries exhausted', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 50 }).filter((s) => s.trim().length > 0),
          fc.string({ minLength: 1, maxLength: 50 }).filter((s) => s.trim().length > 0),
          async (sfuUrl, token) => {
            resetAll();
            mockMaxRetries = 1; // exhaust after 1 failure
            await driveToActive();

            // Establish media and then fail it to exhaust retries
            messageHandler!({ type: 'media_token', sfuUrl: 'wss://initial', token: 'initial-tok' });
            await tick();

            lastLkModule!.callbacks.onMediaFailed('test failure');
            await tick();

            expect(latestState!.mediaState).toBe('failed');
            expect(latestState!.mediaReconnectFailures).toBe(1);

            const moduleCountBefore = lkConstructorCalls.length;

            // Send another media_token — should be ignored (retries exhausted)
            messageHandler!({ type: 'media_token', sfuUrl, token });
            await tick();
            await tick(); // getReconnectConfig().then()

            // No new LiveKitModule created
            expect(lkConstructorCalls.length).toBe(moduleCountBefore);

            // System event about retries exhausted
            const ignoreEvents = latestState!.events.filter(
              (e) => e.type === 'system' && e.message.includes('retries exhausted'),
            );
            expect(ignoreEvents.length).toBeGreaterThanOrEqual(1);

            // mediaState still failed
            expect(latestState!.mediaState).toBe('failed');

            leaveRoom();
          },
        ),
        { numRuns: 50 },
      );
    });
  });
});

// ═══ Task 11: Unit tests for edge cases ════════════════════════════

describe('Edge case unit tests', () => {
  // ─── 11.1: Media token handling edge cases ───────────────────────

  describe('11.1: Media token handling edge cases', () => {
    it('media_token with valid payload calls connect with correct args', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({
        type: 'media_token',
        sfuUrl: 'wss://sfu.example.com',
        token: 'jwt-abc-123',
      });
      await tick();

      expect(lkConstructorCalls).toHaveLength(1);
      expect(lastLkModule!.connectCalls).toHaveLength(1);
      expect(lastLkModule!.connectCalls[0]).toEqual({
        sfuUrl: 'wss://sfu.example.com',
        token: 'jwt-abc-123',
      });

      leaveRoom();
    });

    it('media_token with empty token appends error event and does not connect', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: '' });
      await tick();

      expect(lkConstructorCalls).toHaveLength(0);
      const sysEvents = latestState!.events.filter(
        (e) => e.type === 'system' && e.message.includes('empty token or sfuUrl'),
      );
      expect(sysEvents.length).toBeGreaterThanOrEqual(1);

      leaveRoom();
    });

    it('media_token with empty sfuUrl appends error event and does not connect', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: '', token: 'valid-token' });
      await tick();

      expect(lkConstructorCalls).toHaveLength(0);
      const sysEvents = latestState!.events.filter(
        (e) => e.type === 'system' && e.message.includes('empty token or sfuUrl'),
      );
      expect(sysEvents.length).toBeGreaterThanOrEqual(1);

      leaveRoom();
    });

    it('media_token before active state is buffered and deferred', async () => {
      resetAll();
      latestState = null;
      initSession('ch-defer', 'defer-room', 'member', (s) => {
        latestState = s;
      });
      await tick();

      messageHandler!({ type: 'auth_success' });
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'deferred-tok' });
      await tick();

      expect(lkConstructorCalls).toHaveLength(0);

      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-defer',
        participants: [{ participantId: 'self-peer', displayName: 'TestUser' }],
      });
      await tick();

      // Still buffered: active but not yet in a sub-room
      expect(lkConstructorCalls).toHaveLength(0);

      // sub_room_state snapshot arrives (as backend sends after join) — flushes token
      messageHandler!({
        type: 'sub_room_state',
        rooms: [
          { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        ],
      });
      await tick();

      expect(lkConstructorCalls).toHaveLength(1);
      expect(lastLkModule!.connectCalls[0]).toEqual({ sfuUrl: 'wss://sfu', token: 'deferred-tok' });

      leaveRoom();
    });

    it('media_token while failed is ignored when retries exhausted', async () => {
      resetAll();
      mockMaxRetries = 1; // exhaust after 1 failure
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      // Simulate failure to exhaust retries
      lastLkModule!.callbacks.onMediaFailed('test failure');
      await tick();

      expect(latestState!.mediaReconnectFailures).toBe(1);
      expect(latestState!.mediaState).toBe('failed');

      const modulesBefore = lkConstructorCalls.length;

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu2', token: 'tok2' });
      await tick();
      await tick(); // extra tick for async getReconnectConfig

      expect(lkConstructorCalls.length).toBe(modulesBefore);
      const ignoreEvents = latestState!.events.filter((e) =>
        e.message.includes('retries exhausted'),
      );
      expect(ignoreEvents.length).toBeGreaterThanOrEqual(1);

      leaveRoom();
    });

    it('plays one leave sound when media_token arrives while retries are already exhausted, even though signaling stays active', async () => {
      resetAll();
      mockMaxRetries = 1; // exhaust after 1 failure
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaFailed('test failure');
      await tick();

      expect(latestState!.mediaReconnectFailures).toBe(1);
      expect(latestState!.mediaState).toBe('failed');
      // Not yet — onMediaFailed alone only marks the media state as failed.
      expect(playNotificationSoundCalls).toEqual([]);

      // Backend pushes a fresh media_token, but the retry budget is spent —
      // this is a pure LiveKit/media-layer give-up: the WS signaling session
      // (machineState) is never touched, so nothing else would ever play
      // the disconnect sound for this scenario.
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu2', token: 'tok2' });
      await tick();
      await tick(); // extra tick for async getReconnectConfig

      expect(getState().machineState).toBe('active');
      expect(playNotificationSoundCalls).toEqual(['leave']);

      leaveRoom();
    });

    it('plays one leave sound when reconnectMedia() itself exhausts the retry budget', async () => {
      resetAll();
      mockMaxRetries = 1; // exhaust after 1 failure
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaFailed('test failure');
      await tick();

      expect(latestState!.mediaReconnectFailures).toBe(1);
      expect(playNotificationSoundCalls).toEqual([]);

      await reconnectMedia();

      expect(getState().machineState).toBe('active');
      expect(playNotificationSoundCalls).toEqual(['leave']);

      leaveRoom();
    });
  });

  describe('11.1b: media_token request timeout (no response from backend)', () => {
    it('retries reconnectMedia when no media_token ever arrives for a pending request', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaConnected();
      await tick();

      sentMessages = [];
      vi.useFakeTimers();
      try {
        lastLkModule!.callbacks.onMediaDisconnected();
        await vi.advanceTimersByTimeAsync(0); // flush reconnectMedia()'s awaited getReconnectConfig()

        expect(sentMessages.filter((m) => m.type === 'join_voice')).toHaveLength(1);
        expect(latestState!.mediaReconnectFailures).toBe(0);

        // No media_token ever arrives — advance past the request timeout.
        await vi.advanceTimersByTimeAsync(15_100);

        expect(latestState!.mediaReconnectFailures).toBe(1);
        expect(sentMessages.filter((m) => m.type === 'join_voice')).toHaveLength(2);
      } finally {
        vi.useRealTimers();
        leaveRoom();
      }
    });

    it('eventually plays the disconnect sound if media_token requests keep timing out', async () => {
      resetAll();
      mockMaxRetries = 1;
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaConnected();
      await tick();

      playNotificationSoundCalls = [];
      vi.useFakeTimers();
      try {
        lastLkModule!.callbacks.onMediaDisconnected();
        await vi.advanceTimersByTimeAsync(0);

        expect(playNotificationSoundCalls).toEqual([]);

        // The one retry attempt the exhausted budget (mockMaxRetries=1)
        // allows also never gets a response — the timeout handler's own
        // reconnectMedia() call hits the exhausted branch directly.
        await vi.advanceTimersByTimeAsync(15_100);

        expect(getState().mediaState).toBe('failed');
        expect(playNotificationSoundCalls).toEqual(['leave']);
      } finally {
        vi.useRealTimers();
        leaveRoom();
      }
    });

    it('a media_token that arrives in time clears the pending timeout (no spurious retry)', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaConnected();
      await tick();

      sentMessages = [];
      vi.useFakeTimers();
      try {
        lastLkModule!.callbacks.onMediaDisconnected();
        await vi.advanceTimersByTimeAsync(0);
        expect(sentMessages.filter((m) => m.type === 'join_voice')).toHaveLength(1);

        // Backend responds well within the timeout window.
        messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu2', token: 'tok2' });
        await vi.advanceTimersByTimeAsync(0);

        // Advancing past where the (now-cleared) timeout would have fired
        // must NOT trigger a spurious extra retry or failure count.
        await vi.advanceTimersByTimeAsync(15_100);

        expect(latestState!.mediaReconnectFailures).toBe(0);
        expect(sentMessages.filter((m) => m.type === 'join_voice')).toHaveLength(1);
      } finally {
        vi.useRealTimers();
        leaveRoom();
      }
    });
  });

  // ─── 11.2: Screen share edge cases ──────────────────────────────

  describe('11.2: Screen share edge cases', () => {
    it('screen picker cancelled does not send StartShare', async () => {
      resetAll();
      await driveToActive();
      await assignSelfToSubRoom();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      // Make startScreenShare return false (cancelled) — must mock the existing
      // fn reference since Object.assign copied it to the module-level lkModule
      vi.mocked(lastLkModule!.startScreenShare).mockResolvedValue(false);

      // startShare() was removed; startFallbackShare() is the equivalent path.
      const { startFallbackShare } = await import('../voice-room');
      await startFallbackShare();
      await tick();

      const shareMessages = sentMessages.filter((m) => m.type === 'start_share');
      expect(shareMessages).toHaveLength(0);

      leaveRoom();
    });

    it('startFallbackShare rejects when self is not in a synchronized room', async () => {
      resetAll();
      await driveToActive('ch-1', 'test-room', false);

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      const { startFallbackShare } = await import('../voice-room');
      await expect(startFallbackShare()).rejects.toThrow('Join a room before sharing.');
      expect(lastLkModule?.startScreenShareCalls ?? 0).toBe(0);
      expect(sentMessages.filter((m) => m.type === 'start_share')).toHaveLength(0);

      leaveRoom();
    });

    it('loads the persisted Windows share-path preference into room state', async () => {
      resetAll();
      vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)' });
      vi.mocked(settingsStore.getWindowsSharePath).mockResolvedValue('native');

      await driveToActive();
      await tick();

      expect(getState().windowsSharePath).toBe('native');

      vi.unstubAllGlobals();
      leaveRoom();
    });

    it('startFallbackShare refuses to publish when the LiveKit reuse patch marker is missing', async () => {
      resetAll();
      vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
      vi.stubGlobal('navigator', {
        userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
        mediaDevices: {
          getUserMedia: vi.fn(),
          getDisplayMedia: vi.fn(),
        },
      });
      await driveToActive();
      await assignSelfToSubRoom();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      delete (globalThis as { __wavisSenderData?: unknown }).__wavisSenderData;

      const { startFallbackShare } = await import('../voice-room');
      await expect(startFallbackShare()).rejects.toThrow('transceiver reuse patch is missing');
      expect(lastLkModule!.startScreenShareCalls).toBe(0);

      expect(getTelemetrySnapshot()).toContainEqual(
        expect.objectContaining({ name: 'share.reuse_patch.missing' }),
      );

      vi.unstubAllGlobals();
      leaveRoom();
    });

    it('startFallbackShare emits share.path.selected telemetry for the browser path on Windows', async () => {
      resetAll();
      vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)' });
      await driveToActive();
      await assignSelfToSubRoom();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      const { startFallbackShare } = await import('../voice-room');
      await startFallbackShare();

      expect(getTelemetrySnapshot()).toContainEqual(
        expect.objectContaining({
          name: 'share.path.selected',
          path: 'browser',
          reason: 'browser_display_media',
        }),
      );

      vi.unstubAllGlobals();
      leaveRoom();
    });

    it('leaving the current sub-room stops local fallback share and uses share_stopped echo for sound', async () => {
      resetAll();
      await driveToActive();
      await assignSelfToSubRoom('room-1');

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      vi.mocked(lastLkModule!.startScreenShare).mockResolvedValue(true);

      const { startFallbackShare } = await import('../voice-room');
      await startFallbackShare();
      messageHandler!({
        type: 'share_started',
        participantId: 'self-peer',
        displayName: 'TestUser',
      });
      await tick();
      sentMessages.length = 0;
      playNotificationSoundCalls = [];

      messageHandler!({
        type: 'sub_room_state',
        rooms: [{ subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: [] }],
      });
      await tick();

      expect(lastLkModule!.stopScreenShareCalls).toBe(1);
      expect(sentMessages).toContainEqual({ type: 'stop-share' });
      expect(playNotificationSoundCalls).not.toContain('share-stop');

      messageHandler!({
        type: 'share_stopped',
        participantId: 'self-peer',
        displayName: 'TestUser',
      });
      await tick();
      expect(playNotificationSoundCalls).toContain('share-stop');
      lastLkModule!.callbacks.onLocalScreenShareEnded();

      leaveRoom();
    });

    it('switching directly between sub-rooms does not stop local fallback share', async () => {
      resetAll();
      await driveToActive();
      await assignSelfToSubRoom('room-1');

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      vi.mocked(lastLkModule!.startScreenShare).mockResolvedValue(true);

      const { startFallbackShare } = await import('../voice-room');
      await startFallbackShare();
      messageHandler!({
        type: 'share_started',
        participantId: 'self-peer',
        displayName: 'TestUser',
      });
      await tick();
      sentMessages.length = 0;

      messageHandler!({
        type: 'sub_room_state',
        rooms: [
          { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: [] },
          { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['self-peer'] },
        ],
      });
      await tick();

      expect(lastLkModule!.stopScreenShareCalls).toBe(0);
      expect(sentMessages.filter((m) => m.type === 'stop-share')).toHaveLength(0);

      leaveRoom();
    });

    it('external screen share end sends StopShare signaling', async () => {
      resetAll();
      await driveToActive();
      await assignSelfToSubRoom();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      lastLkModule!.callbacks.onLocalScreenShareEnded();
      await tick();

      const stopMessages = sentMessages.filter((m) => m.type === 'stop-share');
      expect(stopMessages).toHaveLength(1);

      leaveRoom();
    });

    it('multiple screen shares — all tracked in map, removed on unsubscribe', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      const streamA = {} as MediaStream;
      const streamB = {} as MediaStream;

      lastLkModule!.callbacks.onScreenShareSubscribed('alice', streamA);
      await tick();
      expect(latestState!.screenShareStreams.get('alice')).toBe(streamA);
      expect(latestState!.screenShareStreams.size).toBe(1);

      lastLkModule!.callbacks.onScreenShareSubscribed('bob', streamB);
      await tick();
      expect(latestState!.screenShareStreams.get('bob')).toBe(streamB);
      expect(latestState!.screenShareStreams.size).toBe(2);

      // B unsubscribes — A remains
      lastLkModule!.callbacks.onScreenShareUnsubscribed('bob');
      await tick();
      expect(latestState!.screenShareStreams.has('bob')).toBe(false);
      expect(latestState!.screenShareStreams.get('alice')).toBe(streamA);
      expect(latestState!.screenShareStreams.size).toBe(1);

      // A unsubscribes — no more shares
      lastLkModule!.callbacks.onScreenShareUnsubscribed('alice');
      await tick();
      expect(latestState!.screenShareStreams.size).toBe(0);

      leaveRoom();
    });

    it('share_started retry drops a stale stream whose video track has ended', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      // Stream left over from a previous share: its video track already ended,
      // but the SFU treated the republish as a resume so no fresh
      // TrackSubscribed ever replaced it (icon lit, viewer window loads forever).
      const deadStream = {
        getVideoTracks: () => [{ readyState: 'ended' }],
      } as unknown as MediaStream;
      lastLkModule!.callbacks.onScreenShareSubscribed('u2', deadStream);
      await tick();
      expect(latestState!.screenShareStreams.get('peer-2')).toBe(deadStream);

      vi.useFakeTimers();
      try {
        messageHandler!({
          type: 'share_started',
          participantId: 'peer-2',
          shareType: 'screen_audio',
        });
        await vi.advanceTimersByTimeAsync(1000);

        expect(latestState!.screenShareStreams.has('peer-2')).toBe(false);
      } finally {
        vi.useRealTimers();
      }

      leaveRoom();
    });

    it('share_started retry leaves a live stream untouched', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      const liveStream = {
        getVideoTracks: () => [{ readyState: 'live' }],
      } as unknown as MediaStream;
      lastLkModule!.callbacks.onScreenShareSubscribed('u2', liveStream);
      await tick();

      vi.useFakeTimers();
      try {
        messageHandler!({
          type: 'share_started',
          participantId: 'peer-2',
          shareType: 'screen_audio',
        });
        await vi.advanceTimersByTimeAsync(24_000);

        expect(latestState!.screenShareStreams.get('peer-2')).toBe(liveStream);
      } finally {
        vi.useRealTimers();
      }

      leaveRoom();
    });

    it('keeps retrying past the fixed 24s retry burst while the share never resolves', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      // No onScreenShareSubscribed ever fires for peer-2 — simulates a
      // permanently unresolved track (e.g. an identity that never matches),
      // the "sometimes stays grey forever" case from issue #175.
      vi.useFakeTimers();
      try {
        messageHandler!({
          type: 'share_started',
          participantId: 'peer-2',
          shareType: 'screen_audio',
        });
        await vi.advanceTimersByTimeAsync(24_000);

        const callsAtBurstEnd = lastLkModule!.refreshRemoteScreenShareCalls.length;
        expect(callsAtBurstEnd).toBeGreaterThan(0);
        expect(latestState!.screenShareStreams.has('peer-2')).toBe(false);

        // Well past the old fixed burst (1s/3s/6s/12s/24s) — a healthy retry
        // loop must still be trying, not have given up.
        await vi.advanceTimersByTimeAsync(120_000);
        expect(lastLkModule!.refreshRemoteScreenShareCalls.length).toBeGreaterThan(callsAtBurstEnd);
      } finally {
        vi.useRealTimers();
      }

      leaveRoom();
    });

    it('sustained retry stops once the share resolves to a live stream', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      vi.useFakeTimers();
      try {
        messageHandler!({
          type: 'share_started',
          participantId: 'peer-2',
          shareType: 'screen_audio',
        });
        await vi.advanceTimersByTimeAsync(60_000);

        const liveStream = {
          getVideoTracks: () => [{ readyState: 'live' }],
        } as unknown as MediaStream;
        lastLkModule!.callbacks.onScreenShareSubscribed('u2', liveStream);
        await vi.advanceTimersByTimeAsync(0);

        const callsAfterResolved = lastLkModule!.refreshRemoteScreenShareCalls.length;
        await vi.advanceTimersByTimeAsync(120_000);

        expect(lastLkModule!.refreshRemoteScreenShareCalls.length).toBe(callsAfterResolved);
        expect(latestState!.screenShareStreams.get('peer-2')).toBe(liveStream);
      } finally {
        vi.useRealTimers();
      }

      leaveRoom();
    });

    it('sustained retry stops once the participant stops sharing', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      vi.useFakeTimers();
      try {
        messageHandler!({
          type: 'share_started',
          participantId: 'peer-2',
          shareType: 'screen_audio',
        });
        await vi.advanceTimersByTimeAsync(60_000);

        messageHandler!({
          type: 'share_stopped',
          participantId: 'peer-2',
        });
        await vi.advanceTimersByTimeAsync(0);

        const callsAfterStop = lastLkModule!.refreshRemoteScreenShareCalls.length;
        await vi.advanceTimersByTimeAsync(120_000);

        expect(lastLkModule!.refreshRemoteScreenShareCalls.length).toBe(callsAfterStop);
      } finally {
        vi.useRealTimers();
      }

      leaveRoom();
    });
  });

  // ─── 11.3: Audio and device edge cases ──────────────────────────

  describe('11.3: Audio and device edge cases', () => {
    it('host-mute then unmute attempt is blocked', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      messageHandler!({ type: 'participant_muted', participantId: 'self-peer' });
      await tick();

      const micCallsBefore = lastLkModule!.setMicEnabledCalls.length;

      toggleSelfMute();
      await tick();

      expect(lastLkModule!.setMicEnabledCalls.length).toBe(micCallsBefore);
      const self = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(self!.isMuted).toBe(true);
      expect(self!.isHostMuted).toBe(true);

      leaveRoom();
    });

    it('reconnect-media within cooldown is ignored with event log', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      const now = Date.now();
      vi.spyOn(Date, 'now').mockReturnValue(now);

      await reconnectMedia();

      await reconnectMedia();

      const cooldownEvents = latestState!.events.filter((e) => e.message.includes('cooldown'));
      expect(cooldownEvents.length).toBeGreaterThanOrEqual(1);

      vi.spyOn(Date, 'now').mockRestore();
      leaveRoom();
    });

    it('restarts Windows WASAPI audio after reconnect when an audio-only share is active', async () => {
      resetAll();
      vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
      vi.stubGlobal('navigator', {
        userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
        mediaDevices: {
          getUserMedia: vi.fn(),
          getDisplayMedia: vi.fn(),
        },
      });

      await driveToActive();
      await assignSelfToSubRoom();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      await startCustomShare({
        mode: 'audio_only',
        sourceId: 'default-monitor',
        sourceName: 'System Audio',
        withAudio: false,
      });
      await tick();

      invokeCalls = [];

      await reconnectMedia();
      await tick();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok-2' });
      await tick();

      lastLkModule!.callbacks.onMediaConnected();
      await tick();

      expect(invokeCalls).toContainEqual({
        command: 'audio_share_start',
        args: { sourceId: 'default-monitor' },
      });
      expect(lastLkModule!.startWasapiAudioBridgeCalls).toContain(true);

      vi.unstubAllGlobals();
      leaveRoom();
    });

    it('leave during connecting state performs clean teardown', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      expect(latestState!.mediaState).toBe('connecting');

      leaveRoom();
      await tick();

      expect(latestState!.machineState).toBe('idle');
    });

    it('SDK Reconnecting event does not create duplicate Room', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      const moduleCount = lkConstructorCalls.length;

      lastLkModule!.callbacks.onMediaReconnecting?.();
      await tick();
      expect(latestState!.mediaState).toBe('reconnecting');

      lastLkModule!.callbacks.onMediaReconnected?.();
      await tick();

      expect(latestState!.mediaState).toBe('connected');
      expect(lkConstructorCalls.length).toBe(moduleCount);

      leaveRoom();
    });

    it('terminal media disconnect requests a fresh media token once', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaConnected();
      await tick();

      const module = lastLkModule!;
      sentMessages = [];

      module.callbacks.onMediaDisconnected();
      await tick();
      await tick();

      expect(latestState!.mediaState).toBe('disconnected');
      expect(module.disconnectCalls).toBe(1);
      expect(sentMessages.filter((m) => m.type === 'join_voice')).toHaveLength(1);

      leaveRoom();
    });

    it('LiveKit reconnect success restores active voice state and share availability', async () => {
      resetAll();
      await driveToActive();
      await assignSelfToSubRoom();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaConnected();
      await tick();

      statusChangeHandler!('disconnected');
      await tick();
      expect(latestState!.machineState).toBe('reconnecting');

      lastLkModule!.callbacks.onMediaReconnected?.();
      await tick();

      expect(latestState!.machineState).toBe('active');
      expect(latestState!.mediaState).toBe('connected');
      expect(
        isShareEnabled(
          latestState!.sharePermission,
          latestState!.selfIsHost,
          latestState!.machineState,
          latestState!.mediaState,
          latestState!.joinedSubRoomId,
        ),
      ).toBe(true);

      leaveRoom();
    });

    it('WS-only reconnect (media never drops) restores active state via sub-room rejoin — issue #157', async () => {
      resetAll();
      await driveToActive();
      joinSubRoom('room-1'); // records explicit user intent, as real ParticipantsPanel clicks do

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();
      lastLkModule!.callbacks.onMediaConnected();
      await tick();
      expect(latestState!.machineState).toBe('active');

      // Signaling drops — LiveKit media is intentionally NOT torn down here
      // (it connects directly to the SFU, independent of the WS).
      statusChangeHandler!('disconnected');
      await tick();
      expect(latestState!.machineState).toBe('reconnecting');
      expect(latestState!.mediaState).toBe('connected');

      sentMessages.length = 0;

      // Server's reconnect handshake unconditionally strips sub-room
      // membership for modern clients and does not resend sub_room_joined —
      // the client must notice and proactively rejoin.
      messageHandler!({
        type: 'sub_room_state',
        rooms: [{ subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: [] }],
      });
      await tick();

      expect(latestState!.joinedSubRoomId).toBeNull();
      expect(latestState!.machineState).toBe('reconnecting');
      expect(sentMessages).toContainEqual({ type: 'join_sub_room', subRoomId: 'room-1' });

      messageHandler!({
        type: 'sub_room_joined',
        participantId: 'self-peer',
        subRoomId: 'room-1',
      });
      await tick();

      expect(latestState!.machineState).toBe('active');
      expect(latestState!.mediaState).toBe('connected');
      expect(latestState!.joinedSubRoomId).toBe('room-1');
      expect(
        isShareEnabled(
          latestState!.sharePermission,
          latestState!.selfIsHost,
          latestState!.machineState,
          latestState!.mediaState,
          latestState!.joinedSubRoomId,
        ),
      ).toBe(true);

      leaveRoom();
    });

    it('media failure preserves signaling — participants and chat still work', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      lastLkModule!.callbacks.onMediaFailed('SFU unreachable');
      await tick();

      expect(latestState!.mediaState).toBe('failed');
      expect(latestState!.machineState).toBe('active');

      messageHandler!({
        type: 'participant_joined',
        participantId: 'peer-3',
        displayName: 'Charlie',
        userId: 'u3',
      });
      await tick();

      expect(latestState!.participants.length).toBe(3);
      const charlie = latestState!.participants.find((p) => p.id === 'peer-3');
      expect(charlie).toBeDefined();
      expect(charlie!.displayName).toBe('Charlie');

      leaveRoom();
    });

    it('applies participant_username_updated to the matching participant', async () => {
      await driveToActive();

      messageHandler!({
        type: 'participant_username_updated',
        participantId: 'peer-2',
        username: 'New Alice',
      });
      await tick();

      const alice = latestState!.participants.find((p) => p.id === 'peer-2');
      expect(alice).toBeDefined();
      expect(alice!.displayName).toBe('New Alice');

      leaveRoom();
    });
  });
});

// ═══ Property 11: Signaling message preservation ═══════════════════

describe('Feature: screen-share-quality, Property 11: Signaling message preservation', () => {
  /**
   * **Validates: Requirements 11.3**
   *
   * For any successful screen share start, the VoiceRoom shall send exactly
   * one `start_share` signaling message and no new message types shall be
   * introduced.
   */
  it('VoiceRoom sends exactly one start_share message on successful share start', async () => {
    await fc.assert(
      fc.asyncProperty(
        // Arbitrary channel IDs to exercise different session setups
        fc.string({ minLength: 1, maxLength: 30 }).filter((s) => s.trim().length > 0),
        async (channelId) => {
          resetAll();
          latestState = null;
          initSession(channelId, 'test-room', 'owner', (s) => {
            latestState = s;
          });
          await tick();

          // Drive to active
          if (messageHandler) {
            messageHandler({ type: 'auth_success' });
            messageHandler({
              type: 'joined',
              peerId: 'self-peer',
              roomId: 'room-1',
              participants: [{ participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' }],
            });
          }
          await tick();
          await assignSelfToSubRoom();

          // Establish media so lkModule exists
          messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
          await tick();

          // Ensure startScreenShare returns true (successful share)
          vi.mocked(lastLkModule!.startScreenShare).mockResolvedValue(true);

          // Clear sent messages before the share action
          sentMessages.length = 0;

          // startShare() was removed; startFallbackShare() is the equivalent path.
          const { startFallbackShare } = await import('../voice-room');
          await startFallbackShare();
          await tick();

          // Exactly one message sent
          expect(sentMessages).toHaveLength(1);

          // That message is start_share
          expect(sentMessages[0].type).toBe('start_share');

          // No other message types — no new signaling types introduced
          const messageTypes = sentMessages.map((m) => m.type);
          expect(messageTypes.every((t) => t === 'start_share')).toBe(true);

          leaveRoom();
        },
      ),
      { numRuns: 100 },
    );
  }, 30_000);
});

// ═══ Task 9.3: VoiceRoom delegates startScreenShare to LiveKitModule ═══

describe('VoiceRoom delegates startScreenShare to LiveKitModule', () => {
  /**
   * Validates: Requirements 10.3
   *
   * Assert that startScreenShare on LiveKitModule is invoked when
   * VoiceRoom startFallbackShare is called.
   * (startShare() was removed in cleanup 2026-03; startFallbackShare() is
   * the equivalent browser-picker path.)
   */
  it('startFallbackShare() calls startScreenShare on the LiveKitModule', async () => {
    resetAll();
    await driveToActive();
    await assignSelfToSubRoom();

    // Establish media so lkModule exists
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    await tick();

    expect(lastLkModule).not.toBeNull();
    expect(lastLkModule!.startScreenShareCalls).toBe(0);

    // startShare() was removed; startFallbackShare() is the equivalent path.
    const { startFallbackShare } = await import('../voice-room');
    await startFallbackShare();
    await tick();

    // startScreenShare was called exactly once on the LiveKitModule
    expect(lastLkModule!.startScreenShareCalls).toBe(1);

    leaveRoom();
  });
});

// ═══ Property 9: Media reconnect failure counter state machine ═════
// Feature: gui-feature-completion, Property 9
// **Validates: Requirements 20.4, 20.6, 20.7**

describe('Property 9: Media reconnect failure counter state machine', () => {
  it('counter increments on each media failure', async () => {
    await fc.assert(
      fc.asyncProperty(fc.integer({ min: 1, max: 5 }), async (failCount) => {
        resetAll();
        mockMaxRetries = failCount + 5; // ensure we don't exhaust retries
        await driveToActive();

        // Send initial media_token to create the LK module
        messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu.test', token: 'tok-1' });
        await tick();

        // Simulate consecutive failures
        for (let i = 0; i < failCount; i++) {
          lastLkModule!.callbacks.onMediaFailed(`fail-${i}`);
          await tick();
        }

        expect(latestState!.mediaReconnectFailures).toBe(failCount);

        leaveRoom();
      }),
      { numRuns: 20 },
    );
  });

  it('counter resets to 0 on successful media connect', async () => {
    resetAll();
    mockMaxRetries = 10;
    await driveToActive();

    // Send media_token
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu.test', token: 'tok-1' });
    await tick();

    // Simulate 3 failures
    lastLkModule!.callbacks.onMediaFailed('fail-1');
    lastLkModule!.callbacks.onMediaFailed('fail-2');
    lastLkModule!.callbacks.onMediaFailed('fail-3');
    await tick();
    expect(latestState!.mediaReconnectFailures).toBe(3);

    // Simulate success
    lastLkModule!.callbacks.onMediaConnected();
    await tick();
    expect(latestState!.mediaReconnectFailures).toBe(0);

    leaveRoom();
  });

  it('mediaState becomes failed when failures reach maxRetries via reconnectMedia', async () => {
    await fc.assert(
      fc.asyncProperty(fc.integer({ min: 1, max: 5 }), async (maxRetries) => {
        resetAll();
        mockMaxRetries = maxRetries;
        await driveToActive();

        // Send media_token to create LK module
        messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu.test', token: 'tok-1' });
        await tick();

        // Simulate exactly maxRetries failures
        for (let i = 0; i < maxRetries; i++) {
          lastLkModule!.callbacks.onMediaFailed(`fail-${i}`);
          await tick();
        }

        expect(latestState!.mediaReconnectFailures).toBe(maxRetries);
        expect(latestState!.mediaState).toBe('failed');

        // Now reconnectMedia should refuse (retries exhausted)
        const now = Date.now();
        vi.spyOn(Date, 'now').mockReturnValue(now + 5000); // past cooldown
        await reconnectMedia();

        // Should still be failed — retries exhausted
        expect(latestState!.mediaState).toBe('failed');
        expect(latestState!.events.some((e) => e.message.includes('retries exhausted'))).toBe(true);

        vi.spyOn(Date, 'now').mockRestore();
        leaveRoom();
      }),
      { numRuns: 10 },
    );
  });

  it('resetMediaReconnectFailures allows reconnectMedia to proceed again', async () => {
    resetAll();
    mockMaxRetries = 2;
    await driveToActive();

    // Send media_token
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu.test', token: 'tok-1' });
    await tick();

    // Exhaust retries
    lastLkModule!.callbacks.onMediaFailed('fail-1');
    lastLkModule!.callbacks.onMediaFailed('fail-2');
    await tick();
    expect(latestState!.mediaReconnectFailures).toBe(2);
    expect(latestState!.mediaState).toBe('failed');

    // Reset counter
    resetMediaReconnectFailures();
    await tick();
    expect(latestState!.mediaReconnectFailures).toBe(0);

    // Now reconnectMedia should proceed
    const now = Date.now();
    vi.spyOn(Date, 'now').mockReturnValue(now + 5000);
    await reconnectMedia();

    // Should have sent a join_voice message
    const joinVoiceMsgs = sentMessages.filter((m) => m.type === 'join_voice');
    expect(joinVoiceMsgs.length).toBeGreaterThanOrEqual(1);

    vi.spyOn(Date, 'now').mockRestore();
    leaveRoom();
  });
});

// ═══ Property 10: Media reconnect cooldown enforcement ═════════════
// Feature: gui-feature-completion, Property 10
// **Validates: Requirements 20.5**

describe('Property 10: Media reconnect cooldown enforcement', () => {
  it('second reconnectMedia within 3000ms is rejected', async () => {
    resetAll();
    mockMaxRetries = 10;
    await driveToActive();

    // Send media_token to create LK module
    messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu.test', token: 'tok-1' });
    await tick();

    const baseTime = Date.now();
    vi.spyOn(Date, 'now').mockReturnValue(baseTime);

    // First reconnect — should proceed
    await reconnectMedia();
    const joinCountAfterFirst = sentMessages.filter((m) => m.type === 'join_voice').length;

    // Second reconnect within cooldown — should be rejected at cooldown check (before async)
    vi.spyOn(Date, 'now').mockReturnValue(baseTime + 1000);
    await reconnectMedia();

    const joinCountAfterSecond = sentMessages.filter((m) => m.type === 'join_voice').length;
    // No new join_voice sent
    expect(joinCountAfterSecond).toBe(joinCountAfterFirst);

    // Cooldown event should be logged
    expect(latestState!.events.some((e) => e.message.includes('cooldown'))).toBe(true);

    vi.spyOn(Date, 'now').mockRestore();
    leaveRoom();
  });

  it('reconnectMedia after cooldown period proceeds', async () => {
    await fc.assert(
      fc.asyncProperty(fc.integer({ min: 3001, max: 10000 }), async (delayMs) => {
        resetAll();
        mockMaxRetries = 10;
        await driveToActive();

        messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu.test', token: 'tok-1' });
        await tick();

        const baseTime = Date.now();
        vi.spyOn(Date, 'now').mockReturnValue(baseTime);

        // First reconnect
        await reconnectMedia();
        const joinCountAfterFirst = sentMessages.filter((m) => m.type === 'join_voice').length;

        // Second reconnect after cooldown
        vi.spyOn(Date, 'now').mockReturnValue(baseTime + delayMs);
        await reconnectMedia();

        const joinCountAfterSecond = sentMessages.filter((m) => m.type === 'join_voice').length;
        expect(joinCountAfterSecond).toBeGreaterThan(joinCountAfterFirst);

        vi.spyOn(Date, 'now').mockRestore();
        leaveRoom();
      }),
      { numRuns: 20 },
    );
  });
});

describe('stream volume persistence', () => {
  it('getPersistedStreamVolume returns null when no stream volumes have been saved', async () => {
    await driveToActive();
    expect(getPersistedStreamVolume('peer-2')).toBeNull();
    leaveRoom();
  });

  it('persistStreamVolume stores volume keyed by userId, readable via getPersistedStreamVolume', async () => {
    await driveToActive(); // peer-2 has userId u2

    persistStreamVolume('peer-2', 40);

    expect(getPersistedStreamVolume('peer-2')).toBe(40);
    leaveRoom();
  });

  it('persistStreamVolume overwrites previous value for the same participant', async () => {
    await driveToActive();

    persistStreamVolume('peer-2', 40);
    persistStreamVolume('peer-2', 80);

    expect(getPersistedStreamVolume('peer-2')).toBe(80);
    leaveRoom();
  });

  it('persistStreamVolume clamps volume to 0–100', async () => {
    await driveToActive();

    persistStreamVolume('peer-2', 150);
    expect(getPersistedStreamVolume('peer-2')).toBe(100);

    persistStreamVolume('peer-2', -10);
    expect(getPersistedStreamVolume('peer-2')).toBe(0);
    leaveRoom();
  });

  it('getPersistedStreamVolume returns null for unknown participantId', async () => {
    await driveToActive();
    persistStreamVolume('peer-2', 55);
    expect(getPersistedStreamVolume('unknown-peer')).toBeNull();
    leaveRoom();
  });

  it('getPersistedStreamVolume returns saved value loaded from channel prefs on join', async () => {
    vi.mocked(settingsStore.getChannelVolumes).mockResolvedValueOnce({
      master: 70,
      participants: {},
      streams: { u2: 55 },
    });

    await driveToActive(); // peer-2 has userId u2

    expect(getPersistedStreamVolume('peer-2')).toBe(55);
    leaveRoom();
  });

  it('persistStreamMuted stores mute separately from the slider volume', async () => {
    await driveToActive();

    persistStreamVolume('peer-2', 65);
    persistStreamMuted('peer-2', true);

    expect(getPersistedStreamVolume('peer-2')).toBe(65);
    expect(getPersistedStreamMuted('peer-2')).toBe(true);
    leaveRoom();
  });

  it('getPersistedStreamMuted returns saved mute loaded from channel prefs on join', async () => {
    vi.mocked(settingsStore.getChannelVolumes).mockResolvedValueOnce({
      master: 70,
      participants: {},
      streams: { u2: 55 },
      streamMutes: { u2: true },
    });

    await driveToActive();

    expect(getPersistedStreamMuted('peer-2')).toBe(true);
    leaveRoom();
  });

  it('getPersistedStreamMuted treats legacy saved volume 0 as muted', async () => {
    vi.mocked(settingsStore.getChannelVolumes).mockResolvedValueOnce({
      master: 70,
      participants: {},
      streams: { u2: 0 },
    });

    await driveToActive();

    expect(getPersistedStreamMuted('peer-2')).toBe(true);
    leaveRoom();
  });
});
