/**
 * Tests for voice-room.ts dispatchMessage handler — newer message variants.
 *
 * dispatchMessage is private, so we test it indirectly via:
 *   1. initSession() to set up a session with a mock SignalingClient
 *   2. Injecting messages through the registered onMessage handler
 *   3. Observing state changes via getState()
 *
 * Each test calls leaveRoom() in afterEach to reset module-level state.
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import { getState, leaveRoom, initSession } from '../voice-room';

// ─── Mock all Tauri / external dependencies ────────────────────────

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
  emit: vi.fn().mockResolvedValue(undefined),
  once: vi.fn().mockResolvedValue(() => {}),
}));
vi.mock('@tauri-apps/plugin-store', () => ({
  load: vi.fn().mockResolvedValue({
    get: vi.fn().mockResolvedValue(undefined),
    set: vi.fn().mockResolvedValue(undefined),
    save: vi.fn().mockResolvedValue(undefined),
  }),
}));
vi.mock('@features/auth/auth', () => ({
  getServerUrl: vi.fn().mockResolvedValue('http://localhost:3000'),
  getUsername: vi.fn().mockResolvedValue('TestUser'),
  refreshTokens: vi.fn().mockResolvedValue({ status: 'success' }),
  onTokensRefreshed: vi.fn().mockReturnValue(() => {}),
  getAccessToken: vi.fn().mockResolvedValue('test-token'),
  isTokenExpired: vi.fn().mockResolvedValue(false),
}));
vi.mock('@shared/hotkey-bridge', () => ({
  registerMuteHotkey: vi.fn().mockResolvedValue(undefined),
  unregisterMuteHotkey: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../notification-sounds', () => ({
  playNotificationSound: vi.fn().mockResolvedValue(undefined),
  updateCachedNotificationVolume: vi.fn(),
  updateCachedSoundVolumes: vi.fn(),
  prewarmAudioContext: vi.fn(),
}));
vi.mock('../livekit-media', () => ({
  LiveKitModule: vi.fn().mockImplementation(() => ({
    connect: vi.fn().mockResolvedValue(undefined),
    disconnect: vi.fn().mockResolvedValue(undefined),
    setMicEnabled: vi.fn(),
    setMasterVolume: vi.fn(),
    setParticipantVolume: vi.fn(),
    setParticipantPassthrough: vi.fn(),
    setPassthroughFilterSettings: vi.fn(),
    startNativeCapture: vi.fn().mockResolvedValue(undefined),
    stopNativeCapture: vi.fn().mockResolvedValue(undefined),
    replaceNativeCaptureSource: vi.fn().mockResolvedValue(undefined),
    startWasapiAudioBridge: vi.fn().mockResolvedValue(undefined),
    stopWasapiAudioBridge: vi.fn().mockResolvedValue(undefined),
    prepareNativeCapture: vi.fn(),
    prepareNativeCaptureFailureListener: vi.fn().mockResolvedValue(undefined),
    attachWindowsNativeCaptureDiagnostics: vi.fn(),
    startFallbackShare: vi.fn().mockResolvedValue({ started: false, withAudio: false }),
    stopShare: vi.fn().mockResolvedValue(undefined),
    setScreenShareQuality: vi.fn().mockResolvedValue(undefined),
    setSinkId: vi.fn().mockResolvedValue(undefined),
    setAudioGain: vi.fn(),
    setNoiseSuppressionEnabled: vi.fn(),
    on: vi.fn(),
    off: vi.fn(),
  })),
}));
vi.mock('../native-media', () => ({
  NativeMediaModule: vi.fn().mockImplementation(() => ({
    connect: vi.fn().mockResolvedValue(undefined),
    disconnect: vi.fn().mockResolvedValue(undefined),
    setMicEnabled: vi.fn(),
    on: vi.fn(),
    off: vi.fn(),
  })),
}));
vi.mock('../audio-devices', () => ({ setActiveLiveKitModule: vi.fn() }));
vi.mock('../telemetry', () => ({
  emitTelemetryEvent: vi.fn(),
  WindowsSharePath: {},
}));
vi.mock('../motion-detector', () => ({
  MotionDetector: vi.fn(),
  DEFAULT_MOTION_DETECTOR_CONFIG: {},
}));
vi.mock('../motion-sample-source', () => ({
  startOffscreenCanvasSampling: vi.fn(),
  isLeakDiagnosticsThumbnailingActive: vi.fn().mockReturnValue(false),
}));
vi.mock('../share-leak-diagnostics', () => ({}));
vi.mock('../linux-capability-matrix', () => ({ lookupLinuxCapability: vi.fn() }));
vi.mock('@features/settings/settings-store', () => ({
  getDefaultVolume: vi.fn().mockResolvedValue(70),
  getReconnectConfig: vi.fn().mockResolvedValue({ maxRetries: 3 }),
  getMuteHotkey: vi.fn().mockResolvedValue(null),
  getProfileColor: vi.fn().mockResolvedValue(null),
  getChannelVolumes: vi.fn().mockResolvedValue({}),
  getWindowsSharePath: vi.fn().mockResolvedValue('browser'),
  setChannelVolumes: vi.fn().mockResolvedValue(undefined),
  getVideoInputDevice: vi.fn().mockResolvedValue(null),
  setVideoInputDevice: vi.fn().mockResolvedValue(undefined),
  getNotificationVolume: vi.fn().mockResolvedValue(100),
  getSoundVolumes: vi.fn().mockResolvedValue({}),
  DEFAULT_PASSTHROUGH_VOLUME: 20,
}));
// ─── Message injection state ──────────────────────────────────────
// The mock captures the onMessage handler so tests can inject messages directly.
let _messageHandler: ((msg: unknown) => void) | null = null;

vi.mock('@shared/websocket', () => {
  class MockSignalingClient {
    status = 'disconnected';
    connect = vi.fn();
    connectWithAuth = vi.fn().mockResolvedValue(undefined);
    disconnect = vi.fn();
    send = vi.fn();
    onMessage = vi.fn().mockImplementation((h: (msg: unknown) => void) => {
      _messageHandler = h;
      return () => {
        _messageHandler = null;
      };
    });
    onStatusChange = vi.fn().mockImplementation((_h: (s: string) => void) => {
      return () => {};
    });
  }

  return { SignalingClient: MockSignalingClient };
});

// ─── Helpers ──────────────────────────────────────────────────────

async function setupActiveSession(): Promise<void> {
  initSession('ch-1', 'Test Channel', 'member', () => {});
  // Simulate auth_success → joining → joined via the captured handler
  _messageHandler?.({ type: 'auth_success' });
  _messageHandler?.({
    type: 'joined',
    peerId: 'self-peer',
    roomId: 'room-1',
    participants: [],
    sharePermission: 'anyone',
  });
}

function inject(msg: unknown): void {
  _messageHandler?.(msg);
}

afterEach(() => {
  leaveRoom();
  vi.clearAllMocks();
});

// ─── session_displaced ────────────────────────────────────────────

describe('session_displaced', () => {
  it('sets machineState to idle', async () => {
    await setupActiveSession();
    inject({ type: 'session_displaced' });
    expect(getState().machineState).toBe('idle');
  });

  it('sets error message about session takeover', async () => {
    await setupActiveSession();
    inject({ type: 'session_displaced' });
    expect(getState().error).toContain('another device');
  });

  it('appends a system event', async () => {
    await setupActiveSession();
    inject({ type: 'session_displaced' });
    const events = getState().events;
    expect(events.some((e) => e.type === 'system' && e.message.includes('another device'))).toBe(
      true,
    );
  });
});

// ─── participant_deafened / participant_undeafened ─────────────────

describe('participant_deafened', () => {
  it('sets isDeafened on the matching participant', async () => {
    await setupActiveSession();
    inject({
      type: 'participant_joined',
      participantId: 'peer-2',
      displayName: 'Bob',
    });
    inject({ type: 'participant_deafened', participantId: 'peer-2' });
    const p = getState().participants.find((x) => x.id === 'peer-2');
    expect(p?.isDeafened).toBe(true);
  });

  it('appends a deafen event for remote participants', async () => {
    await setupActiveSession();
    inject({ type: 'participant_joined', participantId: 'peer-2', displayName: 'Bob' });
    inject({ type: 'participant_deafened', participantId: 'peer-2' });
    const events = getState().events;
    expect(events.some((e) => e.type === 'deafen' && e.participantId === 'peer-2')).toBe(true);
  });

  it('does not append event for self (server echo)', async () => {
    await setupActiveSession();
    const eventsBefore = getState().events.length;
    inject({ type: 'participant_deafened', participantId: 'self-peer' });
    // Self echo should not add a new event
    expect(getState().events.length).toBe(eventsBefore);
  });
});

describe('participant_undeafened', () => {
  it('clears isDeafened on the matching participant', async () => {
    await setupActiveSession();
    inject({ type: 'participant_joined', participantId: 'peer-2', displayName: 'Bob' });
    inject({ type: 'participant_deafened', participantId: 'peer-2' });
    inject({ type: 'participant_undeafened', participantId: 'peer-2' });
    const p = getState().participants.find((x) => x.id === 'peer-2');
    expect(p?.isDeafened).toBe(false);
  });

  it('appends an undeafen event for remote participants', async () => {
    await setupActiveSession();
    inject({ type: 'participant_joined', participantId: 'peer-2', displayName: 'Bob' });
    inject({ type: 'participant_undeafened', participantId: 'peer-2' });
    const events = getState().events;
    expect(events.some((e) => e.type === 'undeafen' && e.participantId === 'peer-2')).toBe(true);
  });
});

// ─── sub_room_state ───────────────────────────────────────────────

describe('sub_room_state', () => {
  it('synchronizes passthrough permission and defaults it off when absent', async () => {
    await setupActiveSession();
    inject({ type: 'sub_room_state', rooms: [], passthroughEnabled: true });
    expect(getState().passthroughEnabled).toBe(true);

    inject({ type: 'sub_room_state', rooms: [] });
    expect(getState().passthroughEnabled).toBe(false);
  });

  it('populates subRooms from the message', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: ['self-peer'] },
        { subRoomId: 'sr-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
      passthroughVolumePercent: 20,
    });
    const { subRooms } = getState();
    expect(subRooms).toHaveLength(2);
    expect(subRooms[0].id).toBe('sr-1');
    expect(subRooms[1].id).toBe('sr-2');
  });

  it('sets passthrough state when present', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: [] },
        { subRoomId: 'sr-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
      passthrough: { sourceSubRoomId: 'sr-1', targetSubRoomId: 'sr-2', label: '1 - 2' },
      passthroughVolumePercent: 30,
    });
    const { passthrough, passthroughVolume } = getState();
    expect(passthrough?.sourceSubRoomId).toBe('sr-1');
    expect(passthrough?.targetSubRoomId).toBe('sr-2');
    expect(passthroughVolume).toBe(30);
  });

  it('clears passthrough when absent', async () => {
    await setupActiveSession();
    // First set passthrough
    inject({
      type: 'sub_room_state',
      rooms: [],
      passthrough: { sourceSubRoomId: 'sr-1', targetSubRoomId: 'sr-2', label: '1 - 2' },
      passthroughVolumePercent: 20,
    });
    // Then clear it
    inject({ type: 'sub_room_state', rooms: [], passthroughVolumePercent: 20 });
    expect(getState().passthrough).toBeNull();
  });

  it('appends passthrough set event when passthrough becomes active', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [],
      passthrough: { sourceSubRoomId: 'sr-1', targetSubRoomId: 'sr-2', label: '1 - 2' },
      passthroughVolumePercent: 20,
    });
    const events = getState().events;
    expect(events.some((e) => e.type === 'passthrough' && e.message.includes('set'))).toBe(true);
  });

  it('appends passthrough cleared event when passthrough is removed', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [],
      passthrough: { sourceSubRoomId: 'sr-1', targetSubRoomId: 'sr-2', label: '1 - 2' },
      passthroughVolumePercent: 20,
    });
    inject({ type: 'sub_room_state', rooms: [], passthroughVolumePercent: 20 });
    const events = getState().events;
    expect(events.some((e) => e.type === 'passthrough' && e.message === 'cleared')).toBe(true);
  });

  it('clamps passthroughVolumePercent to 0–100', async () => {
    await setupActiveSession();
    inject({ type: 'sub_room_state', rooms: [], passthroughVolumePercent: 150 });
    expect(getState().passthroughVolume).toBe(100);
    inject({ type: 'sub_room_state', rooms: [], passthroughVolumePercent: -10 });
    expect(getState().passthroughVolume).toBe(0);
  });

  it('applies passthrough filter settings from sub_room_state', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [],
      passthroughFiltersEnabled: false,
      passthroughFilterStrength: 150,
    });
    expect(getState().passthroughFiltersEnabled).toBe(false);
    expect(getState().passthroughFilterStrength).toBe(100);
  });

  it('sorts rooms by roomNumber', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'sr-3', roomNumber: 3, isDefault: false, participantIds: [] },
        { subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: [] },
        { subRoomId: 'sr-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
      passthroughVolumePercent: 20,
    });
    const ids = getState().subRooms.map((r) => r.id);
    expect(ids).toEqual(['sr-1', 'sr-2', 'sr-3']);
  });
});

// ─── sub_room_joined ──────────────────────────────────────────────

describe('sub_room_joined', () => {
  it('adds participant to the sub-room', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [{ subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: [] }],
      passthroughVolumePercent: 20,
    });
    inject({ type: 'participant_joined', participantId: 'peer-2', displayName: 'Bob' });
    inject({
      type: 'sub_room_joined',
      participantId: 'peer-2',
      subRoomId: 'sr-1',
      source: 'explicit',
    });
    const room = getState().subRooms.find((r) => r.id === 'sr-1');
    expect(room?.participantIds).toContain('peer-2');
  });

  it('appends a join event', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [{ subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: [] }],
      passthroughVolumePercent: 20,
    });
    inject({ type: 'participant_joined', participantId: 'peer-2', displayName: 'Bob' });
    inject({
      type: 'sub_room_joined',
      participantId: 'peer-2',
      subRoomId: 'sr-1',
      source: 'explicit',
    });
    const events = getState().events;
    expect(events.some((e) => e.type === 'join' && e.participantId === 'peer-2')).toBe(true);
  });
});

// ─── sub_room_left ────────────────────────────────────────────────

describe('sub_room_left', () => {
  it('removes participant from the sub-room', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [{ subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: ['peer-2'] }],
      passthroughVolumePercent: 20,
    });
    inject({ type: 'participant_joined', participantId: 'peer-2', displayName: 'Bob' });
    inject({ type: 'sub_room_left', participantId: 'peer-2', subRoomId: 'sr-1' });
    const room = getState().subRooms.find((r) => r.id === 'sr-1');
    expect(room?.participantIds).not.toContain('peer-2');
  });

  it('appends a leave event', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [{ subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: ['peer-2'] }],
      passthroughVolumePercent: 20,
    });
    inject({ type: 'participant_joined', participantId: 'peer-2', displayName: 'Bob' });
    inject({ type: 'sub_room_left', participantId: 'peer-2', subRoomId: 'sr-1' });
    const events = getState().events;
    expect(events.some((e) => e.type === 'leave' && e.participantId === 'peer-2')).toBe(true);
  });
});

// ─── sub_room_deleted ─────────────────────────────────────────────

describe('sub_room_deleted', () => {
  it('removes the sub-room from state', async () => {
    await setupActiveSession();
    inject({
      type: 'sub_room_state',
      rooms: [
        { subRoomId: 'sr-1', roomNumber: 1, isDefault: true, participantIds: [] },
        { subRoomId: 'sr-2', roomNumber: 2, isDefault: false, participantIds: [] },
      ],
      passthroughVolumePercent: 20,
    });
    inject({ type: 'sub_room_deleted', subRoomId: 'sr-2' });
    expect(getState().subRooms.find((r) => r.id === 'sr-2')).toBeUndefined();
    expect(getState().subRooms).toHaveLength(1);
  });
});

// ─── error message ────────────────────────────────────────────────

describe('error message', () => {
  it('appends a system event with the error message', async () => {
    await setupActiveSession();
    inject({ type: 'error', message: 'something went wrong' });
    const events = getState().events;
    expect(events.some((e) => e.type === 'system' && e.message === 'something went wrong')).toBe(
      true,
    );
  });

  it('sets lastChatError', async () => {
    await setupActiveSession();
    inject({ type: 'error', message: 'chat rate limit exceeded' });
    expect(getState().lastChatError).toBe('chat rate limit exceeded');
  });
});

// ─── chat_history_response ────────────────────────────────────────

describe('chat_history_response', () => {
  it('sets historyLoaded to true', async () => {
    await setupActiveSession();
    inject({ type: 'chat_history_response', messages: [] });
    expect(getState().historyLoaded).toBe(true);
  });

  it('merges history messages into chatMessages', async () => {
    await setupActiveSession();
    inject({
      type: 'chat_history_response',
      messages: [
        {
          messageId: 'hist-1',
          participantId: 'peer-2',
          displayName: 'Bob',
          text: 'hello from history',
          timestamp: '2026-05-01T10:00:00Z',
        },
      ],
    });
    const { chatMessages } = getState();
    expect(chatMessages.some((m) => m.text === 'hello from history')).toBe(true);
  });
});
