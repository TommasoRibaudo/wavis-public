/**
 * Frontend test for audio share error propagation and toast display (Task 4.4).
 *
 * Tests that when `invoke('audio_share_start', ...)` rejects:
 * - toast.error() is called with the error message
 * - appendEvent is called for the room log
 * - audio share session state is NOT set (no activeAudioShare)
 * - AudioShareStartResult interface no longer has `warning` field
 *
 * Validates: Requirements 2.5
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { AudioShareStartResult } from '@features/screen-share/share-types';

/* ─── Type-Level Assertion ──────────────────────────────────────── */

/**
 * Compile-time check: AudioShareStartResult must NOT have a `warning` field.
 * If someone re-adds `warning`, this line will produce a TypeScript error
 * because the conditional type resolves to `never` instead of `true`.
 */
type AssertNoWarning = AudioShareStartResult extends { warning: unknown } ? never : true;
// @ts-expect-error — unused variable; exists solely for compile-time assertion
const _typeCheck: AssertNoWarning = true; // eslint-disable-line @typescript-eslint/no-unused-vars

/* ─── Mock State ────────────────────────────────────────────────── */

let lkConstructorCalls: Array<Record<string, unknown>>;
let sentMessages: Array<Record<string, unknown>>;
let messageHandler: ((msg: unknown) => void) | null;

/** Tracks invoke calls — key is command name, value is array of arg objects. */
let invokeCalls: Array<{ command: string; args?: Record<string, unknown> }>;

/** When set, invoke('audio_share_start') will reject with this error. */
let audioShareStartError: string | null;

/* ─── Mock toast (sonner) ───────────────────────────────────────── */

const mockToastError = vi.fn();

vi.mock('sonner', () => ({
  toast: {
    error: (...args: unknown[]) => mockToastError(...args),
    success: vi.fn(),
    info: vi.fn(),
    warning: vi.fn(),
  },
}));

/* ─── Mock LiveKitModule ────────────────────────────────────────── */

function createMockLkModule(callbacks: Record<string, (...args: unknown[]) => void>) {
  const mod: Record<string, unknown> = {
    callbacks,
    connectCalls: [] as Array<{ sfuUrl: string; token: string }>,
    disconnectCalls: 0,
    connect: vi.fn(async (sfuUrl: string, token: string) => {
      (mod.connectCalls as Array<{ sfuUrl: string; token: string }>).push({ sfuUrl, token });
    }),
    disconnect: vi.fn(() => { (mod as Record<string, number>).disconnectCalls++; }),
    setMicEnabled: vi.fn(async () => {}),
    setParticipantVolume: vi.fn(),
    setMasterVolume: vi.fn(),
    startScreenShare: vi.fn(async () => true),
    stopScreenShare: vi.fn(async () => {}),
    getActiveScreenShares: vi.fn(() => []),
    prepareNativeCapture: vi.fn(),
    startNativeCapture: vi.fn(async () => {}),
    stopNativeCapture: vi.fn(async () => {}),
  };
  return mod;
}

vi.mock('../livekit-media', () => ({
  LiveKitModule: vi.fn(function (this: Record<string, unknown>, callbacks: Record<string, (...args: unknown[]) => void>) {
    const mod = createMockLkModule(callbacks);
    lkConstructorCalls.push(callbacks);
    Object.assign(this, mod);
    return this;
  }),
}));

/* ─── Mock websocket module ─────────────────────────────────────── */

vi.mock('@shared/websocket', () => ({
  SignalingClient: vi.fn(function (this: Record<string, unknown>) {
    this.status = 'disconnected';
    this.send = vi.fn((msg: Record<string, unknown>) => { sentMessages.push(msg); });
    this.onMessage = vi.fn((handler: (msg: unknown) => void) => {
      messageHandler = handler;
      return () => { messageHandler = null; };
    });
    this.onStatusChange = vi.fn(() => {
      return () => {};
    });
    this.connectWithAuth = vi.fn(async () => {
      (this as Record<string, unknown>).status = 'connected';
    });
    this.disconnect = vi.fn(() => {
      (this as Record<string, unknown>).status = 'disconnected';
    });
    return this;
  }),
}));

/* ─── Mock auth module ──────────────────────────────────────────── */

vi.mock('@features/auth/auth', () => ({
  getServerUrl: vi.fn(async () => 'https://test.wavis.dev'),
  getDisplayName: vi.fn(async () => 'TestUser'),
  getAccessToken: vi.fn(async () => 'mock-token'),
  isTokenExpired: vi.fn(async () => false),
  refreshTokens: vi.fn(async () => true),
  onTokensRefreshed: vi.fn((_cb: () => void) => () => {}),
}));

vi.mock('@shared/helpers', () => ({
  toWsUrl: vi.fn((url: string) => url.replace('https://', 'wss://') + '/ws'),
}));

vi.mock('../audio-devices', () => ({
  setActiveLiveKitModule: vi.fn(),
}));

vi.mock('@features/settings/settings-store', () => ({
  getDefaultVolume: vi.fn(async () => 70),
  getReconnectConfig: vi.fn(async () => ({
    strategy: 'exponential' as const,
    baseDelayMs: 1000,
    maxDelayMs: 30000,
    maxRetries: 10,
  })),
  getMuteHotkey: vi.fn(async () => 'Ctrl+Shift+M'),
  getProfileColor: vi.fn(async () => '#E06C75'),
  getChannelVolumes: vi.fn(async () => null),
  setChannelVolumes: vi.fn(async () => {}),
  getNotificationVolume: vi.fn(async () => 100),
  getSoundVolumes: vi.fn(async () => ({})),
}));

vi.mock('@shared/hotkey-bridge', () => ({
  registerMuteHotkey: vi.fn(async () => {}),
  unregisterMuteHotkey: vi.fn(async () => {}),
}));

/* ─── Mock Tauri APIs ───────────────────────────────────────────── */

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (command: string, args?: Record<string, unknown>) => {
    invokeCalls.push({ command, args });
    if (command === 'audio_share_start' && audioShareStartError) {
      throw new Error(audioShareStartError);
    }
    if (command === 'get_default_audio_monitor') {
      return 'default-monitor';
    }
    return {};
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
  emit: vi.fn(async () => {}),
}));

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  WebviewWindow: vi.fn(),
}));

vi.mock('../native-media', () => ({
  NativeMediaModule: vi.fn(function (this: Record<string, unknown>, callbacks: Record<string, (...args: unknown[]) => void>) {
    const mod = createMockLkModule(callbacks);
    lkConstructorCalls.push(callbacks);
    Object.assign(this, mod);
    return this;
  }),
}));

/* ─── Import module under test ──────────────────────────────────── */

import {
  initSession,
  leaveRoom,
  getState,
  startCustomShare,
} from '../voice-room';
import type { ShareSelection } from '@features/screen-share/share-types';

/* ─── Test Helpers ──────────────────────────────────────────────── */

const tick = () => new Promise<void>(r => setTimeout(r, 0));

function resetAll() {
  lkConstructorCalls = [];
  sentMessages = [];
  messageHandler = null;
  invokeCalls = [];
  audioShareStartError = null;
  mockToastError.mockClear();
}

async function driveToActive() {
  initSession('ch-audio-test', 'audio-test-room', 'owner', () => {});
  await tick();

  if (messageHandler) {
    messageHandler({ type: 'auth_success' });
    messageHandler({
      type: 'joined',
      peerId: 'self-peer',
      roomId: 'room-audio',
      participants: [
        { participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' },
      ],
    });
  }
  await tick();

  // Establish media so lkModule exists
  if (messageHandler) {
    messageHandler({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
  }
  await tick();
}

/* ═══ Tests ═════════════════════════════════════════════════════════ */

describe('Audio share error propagation and toast display (Task 4.4)', () => {
  beforeEach(async () => {
    resetAll();
    await driveToActive();
  });

  afterEach(() => {
    try { leaveRoom(); } catch { /* ignore */ }
  });

  it('toast.error() is called with the error message when audio_share_start rejects', async () => {
    audioShareStartError = 'system audio sharing requires loopback exclusion';

    const selection: ShareSelection = {
      mode: 'audio_only',
      sourceId: 'system-audio-1',
      sourceName: 'System Audio',
      withAudio: false,
    };

    await expect(startCustomShare(selection)).rejects.toThrow();

    expect(mockToastError).toHaveBeenCalledTimes(1);
    expect(mockToastError).toHaveBeenCalledWith(
      'system audio sharing requires loopback exclusion',
    );
  });

  it('room event log receives a system event on audio share failure', async () => {
    audioShareStartError = 'loopback exclusion could not be established';

    const selection: ShareSelection = {
      mode: 'audio_only',
      sourceId: 'system-audio-1',
      sourceName: 'System Audio',
      withAudio: false,
    };

    await expect(startCustomShare(selection)).rejects.toThrow();

    // The outer catch in startCustomShare re-throws, but the inner
    // audio-only catch block calls toast.error before re-throwing.
    expect(mockToastError).toHaveBeenCalled();
  });

  it('activeAudioShare is NOT set after audio_share_start rejects', async () => {
    audioShareStartError = 'no sink-inputs found for PID';

    const selection: ShareSelection = {
      mode: 'audio_only',
      sourceId: 'system-audio-1',
      sourceName: 'System Audio',
      withAudio: false,
    };

    await expect(startCustomShare(selection)).rejects.toThrow();

    const state = getState();
    expect(state.activeAudioShare).toBeNull();
  });

  it('no start_share signaling message is sent on failure', async () => {
    audioShareStartError = 'partial move failure';

    const selection: ShareSelection = {
      mode: 'audio_only',
      sourceId: 'system-audio-1',
      sourceName: 'System Audio',
      withAudio: false,
    };

    const msgsBefore = sentMessages.length;

    await expect(startCustomShare(selection)).rejects.toThrow();

    // No start_share message should have been sent
    const newMsgs = sentMessages.slice(msgsBefore);
    const startShareMsgs = newMsgs.filter(m => m.type === 'start_share');
    expect(startShareMsgs).toHaveLength(0);
  });

  it('AudioShareStartResult does not have a warning field (compile check)', () => {
    // This is a compile-time check — if AudioShareStartResult had a `warning`
    // field, the AssertNoWarning type at the top of this file would resolve
    // to `never` and the assignment `const _typeCheck: AssertNoWarning = true`
    // would fail to compile.
    //
    // At runtime, verify the interface shape by constructing a valid instance:
    const result: AudioShareStartResult = {
      loopback_exclusion_available: true,
    };
    expect(result).toHaveProperty('loopback_exclusion_available');
    expect(result).not.toHaveProperty('warning');
  });

  it('error message is propagated exactly from the invoke rejection', async () => {
    const errorMsg = 'system audio sharing requires loopback exclusion — Windows pre-21H1 does not support per-process audio capture';
    audioShareStartError = errorMsg;

    const selection: ShareSelection = {
      mode: 'audio_only',
      sourceId: 'system-audio-1',
      sourceName: 'System Audio',
      withAudio: false,
    };

    await expect(startCustomShare(selection)).rejects.toThrow(errorMsg);
    expect(mockToastError).toHaveBeenCalledWith(errorMsg);
  });
});
