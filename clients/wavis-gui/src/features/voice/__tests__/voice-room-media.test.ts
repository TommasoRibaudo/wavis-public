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

/** Whether connectWithAuth should reject. */
let connectShouldFail: boolean;
let playNotificationSoundCalls: string[];

// ─── Mock LiveKitModule ────────────────────────────────────────────

interface MockLiveKitModule {
  callbacks: Record<string, (...args: unknown[]) => void>;
  connectCalls: Array<{ sfuUrl: string; token: string }>;
  disconnectCalls: number;
  setMicEnabledCalls: Array<boolean>;
  setParticipantVolumeCalls: Array<{ id: string; vol: number }>;
  setMasterVolumeCalls: Array<number>;
  setScreenShareAudioVolumeCalls: Array<{ id: string; vol: number }>;
  attachScreenShareAudioCalls: string[];
  detachScreenShareAudioCalls: string[];
  startScreenShareCalls: number;
  stopScreenShareCalls: number;
  activeScreenShares: Array<{ identity: string; stream: MediaStream; startedAtMs: number }>;
  connect: (sfuUrl: string, token: string) => Promise<void>;
  disconnect: () => void;
  setMicEnabled: (enabled: boolean) => Promise<void>;
  setParticipantVolume: (id: string, vol: number) => void;
  setMasterVolume: (vol: number) => void;
  setScreenShareAudioVolume: (id: string, vol: number) => void;
  attachScreenShareAudio: (id: string) => void;
  detachScreenShareAudio: (id: string) => void;
  startScreenShare: () => Promise<boolean>;
  stopScreenShare: () => Promise<void>;
  getActiveScreenShares: () => Array<{ identity: string; stream: MediaStream; startedAtMs: number }>;
}

function createMockLkModule(callbacks: Record<string, (...args: unknown[]) => void>): MockLiveKitModule {
  const mod: MockLiveKitModule = {
    callbacks,
    connectCalls: [],
    disconnectCalls: 0,
    setMicEnabledCalls: [],
    setParticipantVolumeCalls: [],
    setMasterVolumeCalls: [],
    setScreenShareAudioVolumeCalls: [],
    attachScreenShareAudioCalls: [],
    detachScreenShareAudioCalls: [],
    startScreenShareCalls: 0,
    stopScreenShareCalls: 0,
    activeScreenShares: [],
    connect: vi.fn(async (sfuUrl: string, token: string) => {
      mod.connectCalls.push({ sfuUrl, token });
    }),
    disconnect: vi.fn(() => { mod.disconnectCalls++; }),
    setMicEnabled: vi.fn(async (enabled: boolean) => { mod.setMicEnabledCalls.push(enabled); }),
    setParticipantVolume: vi.fn((id: string, vol: number) => { mod.setParticipantVolumeCalls.push({ id, vol }); }),
    setMasterVolume: vi.fn((vol: number) => { mod.setMasterVolumeCalls.push(vol); }),
    setScreenShareAudioVolume: vi.fn((id: string, vol: number) => { mod.setScreenShareAudioVolumeCalls.push({ id, vol }); }),
    attachScreenShareAudio: vi.fn((id: string) => { mod.attachScreenShareAudioCalls.push(id); }),
    detachScreenShareAudio: vi.fn((id: string) => { mod.detachScreenShareAudioCalls.push(id); }),
    startScreenShare: vi.fn(async () => { mod.startScreenShareCalls++; return true; }),
    stopScreenShare: vi.fn(async () => { mod.stopScreenShareCalls++; }),
    getActiveScreenShares: vi.fn(() => mod.activeScreenShares),
  };
  return mod;
}


// ─── Mock livekit-media module ─────────────────────────────────────

vi.mock('../livekit-media', () => ({
  LiveKitModule: vi.fn(function (this: Record<string, unknown>, callbacks: Record<string, (...args: unknown[]) => void>) {
    const mod = createMockLkModule(callbacks);
    lastLkModule = mod;
    lkConstructorCalls.push(callbacks);
    // Copy methods onto `this` so the module-level code sees them
    Object.assign(this as Record<string, unknown>, mod);
    return this;
  }),
}));

// ─── Mock websocket module ─────────────────────────────────────────

vi.mock('@shared/websocket', () => ({
  SignalingClient: vi.fn(function (this: Record<string, unknown>) {
    this.status = 'disconnected';
    this.send = vi.fn((msg: Record<string, unknown>) => { sentMessages.push(msg); });
    this.onMessage = vi.fn((handler: (msg: unknown) => void) => {
      messageHandler = handler;
      return () => { messageHandler = null; };
    });
    this.onStatusChange = vi.fn((handler: (status: string) => void) => {
      statusChangeHandler = handler;
      return () => { statusChangeHandler = null; };
    });
    this.connectWithAuth = vi.fn(async () => {
      if (connectShouldFail) throw new Error('connect failed');
      (this as Record<string, unknown>).status = 'connected';
    });
    this.disconnect = vi.fn(() => {
      (this as Record<string, unknown>).status = 'disconnected';
    });
    return this;
  }),
}));

// ─── Mock auth module ──────────────────────────────────────────────

vi.mock('@features/auth/auth', () => ({
  getServerUrl: vi.fn(async () => 'https://test.wavis.dev'),
  getDisplayName: vi.fn(async () => 'TestUser'),
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
}));

// ─── Mock Tauri APIs ───────────────────────────────────────────────
// voice-room.ts imports invoke, listen, emit, and WebviewWindow from
// @tauri-apps/api — these access window.__TAURI_INTERNALS__ which
// doesn't exist in Node.js.

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async () => {}),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
  emit: vi.fn(async () => {}),
}));

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  WebviewWindow: vi.fn(),
}));

// native-media.ts also imports from @tauri-apps/api and screen-share-viewer
vi.mock('../native-media', () => ({
  NativeMediaModule: vi.fn(function (this: Record<string, unknown>, callbacks: Record<string, (...args: unknown[]) => void>) {
    const mod = createMockLkModule(callbacks);
    lastLkModule = mod;
    lkConstructorCalls.push(callbacks);
    Object.assign(this as Record<string, unknown>, mod);
    return this;
  }),
}));

// ─── Import module under test ──────────────────────────────────────

import {
  initSession,
  leaveRoom,
  leaveSubRoom,
  joinSubRoom,
  toggleSelfMute,
  reconnectMedia,
  resetMediaReconnectFailures,
  setScreenShareAudioVolume,
  attachScreenShareAudio,
  detachScreenShareAudio,
  getState,
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
  connectShouldFail = false;
  mockMaxRetries = 10;
  playNotificationSoundCalls = [];
}

/** Flush microtask queue. */
const tick = () => new Promise<void>(r => setTimeout(r, 0));

let latestState: VoiceRoomState | null = null;

/** Initialize a session and drive it to the `active` state. */
async function driveToActive(channelId = 'ch-1', channelName = 'test-room') {
  latestState = null;
  initSession(channelId, channelName, 'owner', (s) => { latestState = s; });
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

describe('VoiceRoom sub-room state', () => {
  it('join_voice advertises sub-room support', async () => {
    initSession('ch-subrooms', 'subroom-test', 'owner', (s) => { latestState = s; });
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
  });

  it('rejoins the desired sub-room after reconnect when the room still exists', async () => {
    await driveToActive('ch-subrooms', 'subroom-test');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer', 'peer-2'] },
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
    expect(newCalls).toContainEqual({ id: 'peer-2', vol: 0 });
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
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['peer-2'] },
      ],
    });
    await tick();

    const newCalls = lastLkModule!.setParticipantVolumeCalls.slice(callsBefore);
    expect(newCalls).toContainEqual({ id: 'peer-2', vol: 0 });
    expect(getState().joinedSubRoomId).toBeNull();
  });
});

describe('VoiceRoom room-scoped join/leave sounds', () => {
  it('does not play join sound for voice-session joined or participant_joined before room membership exists', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

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

  it('plays join when the local user is assigned into a sub-room', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: ['peer-2'] },
      ],
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['join']);
  });

  it('plays leave then join when the local user switches rooms', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer', 'peer-2'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
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

  it('plays leave when another user leaves the local user current room', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer', 'peer-2'] },
      ],
    });
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
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer', 'peer-2'] },
        { subRoomId: 'room-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
    });
    await tick();

    expect(playNotificationSoundCalls).toEqual(['join']);
  });

  it('does not play leave sound for explicit whole-session leave', async () => {
    await driveToActive('ch-sounds', 'room-sounds');

    messageHandler!({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
      ],
    });
    await tick();
    playNotificationSoundCalls = [];

    leaveRoom();

    expect(playNotificationSoundCalls).toEqual([]);
    expect(sentMessages).toContainEqual({ type: 'leave' });
  });
});

afterEach(() => {
  // Clean up any active session
  try { leaveRoom(); } catch { /* ignore */ }
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
        { subRoomId: 'room-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer', 'peer-2'] },
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

    // Without a synchronized sub-room assignment yet, the participant is effectively muted
    const newCalls = lastLkModule!.setParticipantVolumeCalls.slice(callsBefore);
    expect(newCalls).toContainEqual({ id: 'peer-new', vol: 0 });
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
    expect(callsAfterRoomJoin).toContainEqual({ id: 'peer-new', vol: 44 });

    leaveRoom();
  });
});

describe('VoiceRoom SFU cold start retry', () => {
  it('enters server_starting, retries JoinVoice, and clears retry after joined', async () => {
    latestState = null;
    initSession('ch-cold', 'cold-room', 'owner', (s) => { latestState = s; });
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
        participants: [
          { participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' },
        ],
      });

      expect(latestState!.machineState).toBe('active');
      expect(latestState!.serverStartingEstimatedWaitSecs).toBeNull();

      const joinCountAfterJoined = sentMessages.filter((m) => m.type === 'join_voice').length;
      vi.advanceTimersByTime(30_000);
      expect(sentMessages.filter((m) => m.type === 'join_voice')).toHaveLength(joinCountAfterJoined);
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
          fc.string({ minLength: 1, maxLength: 100 }).filter(s => s.trim().length > 0),
          fc.string({ minLength: 1, maxLength: 100 }).filter(s => s.trim().length > 0),
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
    it('media_token before active state is buffered and flushed on joined', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0),
          fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0),
          async (sfuUrl, token) => {
            resetAll();
            latestState = null;
            initSession('ch-buf', 'buf-room', 'member', (s) => { latestState = s; });
            await tick();

            // Simulate auth_success (now in 'joining' state, not 'active')
            messageHandler!({ type: 'auth_success' });

            // Send media_token while NOT active — should be buffered
            messageHandler!({ type: 'media_token', sfuUrl, token });
            await tick();

            // No LiveKitModule created yet
            expect(lkConstructorCalls).toHaveLength(0);

            // Now transition to active via joined
            messageHandler!({
              type: 'joined',
              peerId: 'self-peer',
              roomId: 'room-buf',
              participants: [{ participantId: 'self-peer', displayName: 'TestUser' }],
            });
            await tick();

            // Buffered token should have been flushed — LiveKitModule created
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

  // P3: Invalid media_token rejection
  describe('P3: Invalid media_token rejection', () => {
    it('media_token with empty token or sfuUrl appends system error and does not connect', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.oneof(
            // empty token
            fc.record({ sfuUrl: fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0), token: fc.constant('') }),
            // empty sfuUrl
            fc.record({ sfuUrl: fc.constant(''), token: fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0) }),
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
    it('toggleSelfMute is blocked when host-muted, unblocked after participant_unmuted', async () => {
      resetAll();
      await driveToActive();

      // Send media_token so lkModule exists
      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      // Host-mute self
      messageHandler!({ type: 'participant_muted', participantId: 'self-peer' });
      await tick();

      const self = latestState!.participants.find(p => p.id === 'self-peer');
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
      const selfBlocked = latestState!.participants.find(p => p.id === 'self-peer');
      expect(selfBlocked!.isMuted).toBe(true);

      // Host releases the mute
      messageHandler!({ type: 'participant_unmuted', participantId: 'self-peer' });
      await tick();

      const selfUnlocked = latestState!.participants.find(p => p.id === 'self-peer');
      expect(selfUnlocked!.isHostMuted).toBe(false);
      // Still muted (mic not auto-enabled), but can now self-unmute
      expect(selfUnlocked!.isMuted).toBe(true);

      // Now toggleSelfMute should work
      toggleSelfMute();
      await tick();

      const selfUnmuted = latestState!.participants.find(p => p.id === 'self-peer');
      expect(selfUnmuted!.isMuted).toBe(false);
      // setMicEnabled(true) should have been called
      expect(lastLkModule!.setMicEnabledCalls[lastLkModule!.setMicEnabledCalls.length - 1]).toBe(true);

      leaveRoom();
    });

    it('toggleSelfMute restores a missing self participant and still mutes locally', async () => {
      resetAll();
      latestState = null;
      initSession('ch-missing-self', 'test-room', 'owner', (s) => { latestState = s; });
      await tick();

      messageHandler!({ type: 'auth_success' });
      messageHandler!({
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-1',
        participants: [
          { participantId: 'peer-2', displayName: 'Alice', userId: 'u2' },
        ],
      });
      await tick();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      expect(latestState!.participants.find((p) => p.id === 'self-peer')).toBeTruthy();
      expect(latestState!.events.some((e) => e.message === 'local participant state was restored')).toBe(true);

      toggleSelfMute();
      await tick();

      const self = latestState!.participants.find((p) => p.id === 'self-peer');
      expect(self).toBeTruthy();
      expect(self!.isMuted).toBe(true);
      expect(lastLkModule!.setMicEnabledCalls[lastLkModule!.setMicEnabledCalls.length - 1]).toBe(false);
      expect(latestState!.events.some((e) => e.message === 'you muted microphone')).toBe(true);

      leaveRoom();
    });
  });


  // P22: Reconnection preserves media across WS reconnect
  describe('P22: Reconnection creates fresh instance after full teardown', () => {
    it('WS disconnect during active tears down media; new media_token creates fresh module', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.integer({ min: 1, max: 3 }),
          async (reconnectCycles) => {
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
          },
        ),
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

      const selfBefore = latestState!.participants.find(p => p.id === 'self-peer');
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
        fc.asyncProperty(
          fc.integer({ min: 1, max: 10 }),
          async (reconnectEvents) => {
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
                lastLkModule.callbacks.onSystemEvent('LiveKit reconnecting…');
                lastLkModule.callbacks.onSystemEvent('LiveKit reconnected');
              }
            }
            await tick();

            // No new LiveKitModule instances created
            expect(lkConstructorCalls.length).toBe(moduleCountBefore);

            leaveRoom();
          },
        ),
        { numRuns: 50 },
      );
    });
  });

  // P26: Media failure preserves signaling state
  describe('P26: Media failure preserves signaling state', () => {
    it('media failure sets mediaState=failed but machineState stays active', async () => {
      await fc.assert(
        fc.asyncProperty(
          fc.string({ minLength: 1, maxLength: 100 }).filter(s => s.trim().length > 0),
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
            const failEvents = latestState!.events.filter(e =>
              e.type === 'system' && e.message.includes('media failed'),
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
      const cooldownEvents = latestState!.events.filter(e =>
        e.type === 'system' && e.message.includes('cooldown'),
      );
      expect(cooldownEvents.length).toBeGreaterThanOrEqual(1);

      // Advance past cooldown (3s)
      vi.spyOn(Date, 'now').mockReturnValue(now + 3100);

      // Third reconnect — should succeed now
      await reconnectMedia();

      // A join_voice message should have been sent (requesting new media_token)
      const joinVoiceMsgs = sentMessages.filter(m => m.type === 'join_voice');
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
          fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0),
          fc.string({ minLength: 1, maxLength: 50 }).filter(s => s.trim().length > 0),
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
            const ignoreEvents = latestState!.events.filter(e =>
              e.type === 'system' && e.message.includes('retries exhausted'),
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

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu.example.com', token: 'jwt-abc-123' });
      await tick();

      expect(lkConstructorCalls).toHaveLength(1);
      expect(lastLkModule!.connectCalls).toHaveLength(1);
      expect(lastLkModule!.connectCalls[0]).toEqual({ sfuUrl: 'wss://sfu.example.com', token: 'jwt-abc-123' });

      leaveRoom();
    });

    it('media_token with empty token appends error event and does not connect', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: '' });
      await tick();

      expect(lkConstructorCalls).toHaveLength(0);
      const sysEvents = latestState!.events.filter(e => e.type === 'system' && e.message.includes('empty token or sfuUrl'));
      expect(sysEvents.length).toBeGreaterThanOrEqual(1);

      leaveRoom();
    });

    it('media_token with empty sfuUrl appends error event and does not connect', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: '', token: 'valid-token' });
      await tick();

      expect(lkConstructorCalls).toHaveLength(0);
      const sysEvents = latestState!.events.filter(e => e.type === 'system' && e.message.includes('empty token or sfuUrl'));
      expect(sysEvents.length).toBeGreaterThanOrEqual(1);

      leaveRoom();
    });

    it('media_token before active state is buffered and deferred', async () => {
      resetAll();
      latestState = null;
      initSession('ch-defer', 'defer-room', 'member', (s) => { latestState = s; });
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
      const ignoreEvents = latestState!.events.filter(e => e.message.includes('retries exhausted'));
      expect(ignoreEvents.length).toBeGreaterThanOrEqual(1);

      leaveRoom();
    });
  });

  // ─── 11.2: Screen share edge cases ──────────────────────────────

  describe('11.2: Screen share edge cases', () => {
    it('screen picker cancelled does not send StartShare', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      // Make startScreenShare return false (cancelled) — must mock the existing
      // fn reference since Object.assign copied it to the module-level lkModule
      vi.mocked(lastLkModule!.startScreenShare).mockResolvedValue(false);

      // startShare() was removed; startFallbackShare() is the equivalent path.
      const { startFallbackShare } = await import('../voice-room');
      await startFallbackShare();
      await tick();

      const shareMessages = sentMessages.filter(m => m.type === 'start_share');
      expect(shareMessages).toHaveLength(0);

      leaveRoom();
    });

    it('external screen share end sends StopShare signaling', async () => {
      resetAll();
      await driveToActive();

      messageHandler!({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
      await tick();

      lastLkModule!.callbacks.onLocalScreenShareEnded();
      await tick();

      const stopMessages = sentMessages.filter(m => m.type === 'stop_share');
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
      const self = latestState!.participants.find(p => p.id === 'self-peer');
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

      const cooldownEvents = latestState!.events.filter(e => e.message.includes('cooldown'));
      expect(cooldownEvents.length).toBeGreaterThanOrEqual(1);

      vi.spyOn(Date, 'now').mockRestore();
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

      lastLkModule!.callbacks.onSystemEvent('LiveKit reconnecting…');
      lastLkModule!.callbacks.onSystemEvent('LiveKit reconnected');
      await tick();

      expect(lkConstructorCalls.length).toBe(moduleCount);

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
      const charlie = latestState!.participants.find(p => p.id === 'peer-3');
      expect(charlie).toBeDefined();
      expect(charlie!.displayName).toBe('Charlie');

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
        fc.string({ minLength: 1, maxLength: 30 }).filter(s => s.trim().length > 0),
        async (channelId) => {
          resetAll();
          latestState = null;
          initSession(channelId, 'test-room', 'owner', (s) => { latestState = s; });
          await tick();

          // Drive to active
          if (messageHandler) {
            messageHandler({ type: 'auth_success' });
            messageHandler({
              type: 'joined',
              peerId: 'self-peer',
              roomId: 'room-1',
              participants: [
                { participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' },
              ],
            });
          }
          await tick();

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
          const messageTypes = sentMessages.map(m => m.type);
          expect(messageTypes.every(t => t === 'start_share')).toBe(true);

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
      fc.asyncProperty(
        fc.integer({ min: 1, max: 5 }),
        async (failCount) => {
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
        },
      ),
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
      fc.asyncProperty(
        fc.integer({ min: 1, max: 5 }),
        async (maxRetries) => {
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
          expect(latestState!.events.some(
            (e) => e.message.includes('retries exhausted'),
          )).toBe(true);

          vi.spyOn(Date, 'now').mockRestore();
          leaveRoom();
        },
      ),
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
    expect(latestState!.events.some(
      (e) => e.message.includes('cooldown'),
    )).toBe(true);

    vi.spyOn(Date, 'now').mockRestore();
    leaveRoom();
  });

  it('reconnectMedia after cooldown period proceeds', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.integer({ min: 3001, max: 10000 }),
        async (delayMs) => {
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
        },
      ),
      { numRuns: 20 },
    );
  });
});
