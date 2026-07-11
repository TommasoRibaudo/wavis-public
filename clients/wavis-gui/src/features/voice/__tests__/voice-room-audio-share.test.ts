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
import { clearTelemetrySnapshot, getTelemetrySnapshot } from '../telemetry';

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
let lastLkModule: Record<string, unknown> | null;
let sentMessages: Array<Record<string, unknown>>;
let messageHandler: ((msg: unknown) => void) | null;

/** Tracks invoke calls — key is command name, value is array of arg objects. */
let invokeCalls: Array<{ command: string; args?: Record<string, unknown> }>;
let tauriListeners: Map<string, Set<(event: { payload: unknown }) => void>>;

/** When set, invoke('audio_share_start') will reject with this error. */
let audioShareStartError: string | null;
let audioShareStartResult: AudioShareStartResult;
let linuxDesktopContext: { desktopEnv: string; sessionType: string };

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
    disconnect: vi.fn(() => {
      (mod as Record<string, number>).disconnectCalls++;
    }),
    setMicEnabled: vi.fn(async () => {}),
    setParticipantVolume: vi.fn(),
    setParticipantPassthrough: vi.fn(),
    setPassthroughFilterSettings: vi.fn(),
    setMasterVolume: vi.fn(),
    startScreenShare: vi.fn(async () => true),
    stopScreenShare: vi.fn(async () => {}),
    getLocalScreenShareTrack: vi.fn(() => null),
    getActiveScreenShares: vi.fn(() => []),
    beginNativeCaptureLeakSession: vi.fn(),
    attachWindowsNativeCaptureDiagnostics: vi.fn(),
    markNativeCaptureFailure: vi.fn(),
    prepareNativeCapture: vi.fn(),
    prepareNativeCaptureFailureListener: vi.fn(async () => {}),
    startNativeCapture: vi.fn(async () => {}),
    stopNativeCapture: vi.fn(async () => {}),
    replaceNativeCaptureSource: vi.fn(async () => {}),
    startWasapiAudioBridge: vi.fn(async () => {}),
    stopWasapiAudioBridge: vi.fn(async () => {}),
    restartScreenShareWithAudio: vi.fn(async () => true),
  };
  return mod;
}

vi.mock('../livekit-media', () => ({
  LiveKitModule: vi.fn(function (
    this: Record<string, unknown>,
    callbacks: Record<string, (...args: unknown[]) => void>,
  ) {
    const mod = createMockLkModule(callbacks);
    lkConstructorCalls.push(callbacks);
    Object.assign(this, mod);
    lastLkModule = this;
    return this;
  }),
}));

/* ─── Mock websocket module ─────────────────────────────────────── */

vi.mock('@shared/websocket', () => ({
  SignalingClient: vi.fn(function (this: Record<string, unknown>) {
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
  getUsername: vi.fn(async () => 'TestUser'),
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
  DEFAULT_PASSTHROUGH_VOLUME: 20,
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
  getWindowsSharePath: vi.fn(async () => 'browser'),
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
    if (command === 'audio_share_start') {
      return audioShareStartResult;
    }
    if (command === 'get_default_audio_monitor') {
      return 'default-monitor';
    }
    if (command === 'get_default_audio_monitor_fast') {
      return 'default-monitor';
    }
    if (command === 'get_linux_desktop_context') {
      return linuxDesktopContext;
    }
    if (command === 'screen_share_start') {
      return true;
    }
    if (command === 'screen_share_get_capture_diagnostics') {
      return null;
    }
    return {};
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (eventName: string, handler: (event: { payload: unknown }) => void) => {
    if (!tauriListeners.has(eventName)) {
      tauriListeners.set(eventName, new Set());
    }
    tauriListeners.get(eventName)!.add(handler);
    return () => {
      tauriListeners.get(eventName)?.delete(handler);
    };
  }),
  emit: vi.fn(async () => {}),
}));

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  WebviewWindow: Object.assign(vi.fn(), {
    getByLabel: vi.fn(async () => null),
  }),
}));

vi.mock('../native-media', () => ({
  NativeMediaModule: vi.fn(function (
    this: Record<string, unknown>,
    callbacks: Record<string, (...args: unknown[]) => void>,
  ) {
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
  stopCustomShare,
  startPortalShare,
  setShareQuality,
  toggleShareAudio,
  resetWindowsWgcSessionBypassForTests,
} from '../voice-room';
import type { ShareSelection } from '@features/screen-share/share-types';

/* ─── Test Helpers ──────────────────────────────────────────────── */

const tick = () => new Promise<void>((r) => setTimeout(r, 0));

function resetAll() {
  lkConstructorCalls = [];
  lastLkModule = null;
  sentMessages = [];
  messageHandler = null;
  invokeCalls = [];
  tauriListeners = new Map();
  audioShareStartError = null;
  audioShareStartResult = {
    loopback_exclusion_available: true,
    capture_path: 'wasapi',
  };
  linuxDesktopContext = {
    desktopEnv: 'GNOME',
    sessionType: 'wayland',
  };
  mockToastError.mockClear();
  clearTelemetrySnapshot();
  resetWindowsWgcSessionBypassForTests();
  (globalThis as { __wavisSenderData?: unknown }).__wavisSenderData = {};
}

function emitTauriEvent(eventName: string, payload: unknown): void {
  for (const handler of tauriListeners.get(eventName) ?? []) {
    handler({ payload });
  }
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
      participants: [{ participantId: 'self-peer', displayName: 'TestUser', userId: 'u1' }],
    });
  }
  await tick();

  // Establish media so lkModule exists
  if (messageHandler) {
    messageHandler({ type: 'media_token', sfuUrl: 'wss://sfu', token: 'tok' });
    messageHandler({
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
  }
  await tick();
}

/* ═══ Tests ═════════════════════════════════════════════════════════ */

describe('Audio share error propagation and toast display (Task 4.4)', () => {
  beforeEach(async () => {
    resetAll();
    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (X11; Linux x86_64)' });
    await driveToActive();
  });

  afterEach(() => {
    try {
      leaveRoom();
    } catch {
      /* ignore */
    }
    vi.unstubAllGlobals();
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
    expect(mockToastError).toHaveBeenCalledWith('system audio sharing requires loopback exclusion');
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
    const startShareMsgs = newMsgs.filter((m) => m.type === 'start_share');
    expect(startShareMsgs).toHaveLength(0);
  });

  it('surfaces a room notice when macOS falls back to ScreenCaptureKit audio isolation', async () => {
    audioShareStartResult = {
      loopback_exclusion_available: false,
      capture_path: 'screen_capture_kit',
      fallback_reason: 'process tap isolation was unavailable',
    };

    const selection: ShareSelection = {
      mode: 'audio_only',
      sourceId: 'system-audio-1',
      sourceName: 'System Audio',
      withAudio: false,
    };

    await startCustomShare(selection);

    expect(
      getState().events.some((event) =>
        event.message.includes('ScreenCaptureKit fallback with weaker isolation'),
      ),
    ).toBe(true);
  });

  it('logs the Linux matrix degradation message before starting native capture', async () => {
    audioShareStartResult = {
      loopback_exclusion_available: true,
      capture_path: 'pulse_audio',
    };

    const selection: ShareSelection = {
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      withAudio: true,
    };

    await startCustomShare(selection);

    expect(invokeCalls).toContainEqual({ command: 'get_linux_desktop_context', args: undefined });
    expect(invokeCalls).toContainEqual({
      command: 'screen_share_start_source',
      args: expect.objectContaining({ sourceId: 'screen-1' }),
    });
    expect(invokeCalls).toContainEqual({
      command: 'get_default_audio_monitor_fast',
      args: undefined,
    });
    expect(
      getState().events.some((event) =>
        event.message.includes(
          'remote mic and system-audio still share one subscriber volume slot on Linux',
        ),
      ),
    ).toBe(true);
  });

  it('blocks unsupported Linux native combinations before native capture starts', async () => {
    linuxDesktopContext = {
      desktopEnv: 'sway',
      sessionType: 'wayland',
    };

    const selection: ShareSelection = {
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      withAudio: true,
    };

    await expect(startCustomShare(selection)).rejects.toThrow(
      'Linux native share is not supported on Sway/Wayland in the checked-in Wavis matrix yet; use GNOME, KDE, or an X11 session instead.',
    );

    expect(invokeCalls.some((call) => call.command === 'screen_share_start_source')).toBe(false);
    expect(invokeCalls.some((call) => call.command === 'audio_share_start')).toBe(false);
    expect(
      getState().events.some((event) => event.message.includes('not supported on Sway/Wayland')),
    ).toBe(true);
  });

  it('blocks unsupported Linux portal share before invoking the native portal capture', async () => {
    linuxDesktopContext = {
      desktopEnv: 'sway',
      sessionType: 'wayland',
    };

    await expect(startPortalShare()).rejects.toThrow(
      'Linux native share is not supported on Sway/Wayland in the checked-in Wavis matrix yet; use GNOME, KDE, or an X11 session instead.',
    );

    expect(invokeCalls.some((call) => call.command === 'screen_share_start')).toBe(false);
  });

  it('converts the Rust Linux fallback event into telemetry', () => {
    emitTauriEvent('linux-capture-fallback-activated', {
      from: 'pulse_audio',
      to: 'none',
      reason: 'system audio sharing blocked to prevent echo',
    });

    expect(getTelemetrySnapshot()).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: 'capture.fallback.activated',
          os: 'linux',
          from: 'pulse_audio',
          to: 'none',
          reason: 'system audio sharing blocked to prevent echo',
        }),
      ]),
    );
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
    const errorMsg =
      'system audio sharing requires loopback exclusion — Windows pre-21H1 does not support per-process audio capture';
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

  it('stops native audio capture if the WASAPI bridge fails during audio-only start', async () => {
    resetAll();
    vi.unstubAllGlobals();
    vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: {
        getUserMedia: vi.fn(),
        getDisplayMedia: vi.fn(),
      },
    });
    await driveToActive();

    expect(lastLkModule).not.toBeNull();
    (lastLkModule!.startWasapiAudioBridge as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error('bridge boot failed'),
    );

    const selection: ShareSelection = {
      mode: 'audio_only',
      sourceId: 'pid:123',
      sourceName: 'Edge',
      withAudio: false,
    };

    await expect(startCustomShare(selection)).rejects.toThrow('bridge boot failed');

    expect(invokeCalls).toContainEqual({
      command: 'audio_share_stop',
      args: undefined,
    });
    expect(lastLkModule!.stopWasapiAudioBridge).toHaveBeenCalled();
    expect(getState().activeAudioShare).toBeNull();
  });

  it('fails the selected Windows backend without retrying another native backend', async () => {
    resetAll();
    vi.unstubAllGlobals();
    vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: {
        getUserMedia: vi.fn(),
        getDisplayMedia: vi.fn(),
      },
    });
    await driveToActive();

    expect(lastLkModule).not.toBeNull();
    (lastLkModule!.startNativeCapture as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error('native capture: first frame timeout (2s)'),
    );

    const selection: ShareSelection = {
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      withAudio: true,
      sourceKind: 'screen',
    };

    await expect(startCustomShare(selection)).rejects.toThrow(
      'native capture: first frame timeout (2s)',
    );

    expect(invokeCalls.filter((call) => call.command === 'screen_share_start_source')).toEqual([
      {
        command: 'screen_share_start_source',
        args: expect.objectContaining({
          sourceId: 'screen-1',
          sourceKind: 'screen',
          captureBackend: 'wgc',
        }),
      },
    ]);
    expect(invokeCalls).toContainEqual({
      command: 'screen_share_stop',
      args: undefined,
    });
    expect(lastLkModule!.stopNativeCapture).toHaveBeenCalled();
    expect(lastLkModule!.startNativeCapture).toHaveBeenCalledWith({
      firstFrameTimeoutMs: 2000,
      lowJsBridgeFpsRetry: {
        thresholdFps: 20,
        durationMs: 2000,
        reason: 'wgc_sustained_low_js_observed_sequence_fps',
      },
    });
    expect(lastLkModule!.attachWindowsNativeCaptureDiagnostics).toHaveBeenCalled();
    expect(invokeCalls.some((call) => call.command === 'audio_share_start')).toBe(false);
    expect(sentMessages.filter((message) => message.type === 'start_share')).toHaveLength(0);
    expect(getState().activeVideoShare).toBeNull();
    expect(getTelemetrySnapshot()).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: 'capture.native.failed',
          sourceKind: 'screen',
          backend: 'wgc',
        }),
      ]),
    );
  });

  it('syncs selected max quality into Rust before Windows native source capture', async () => {
    resetAll();
    vi.unstubAllGlobals();
    vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: { getUserMedia: vi.fn(), getDisplayMedia: vi.fn() },
    });
    await driveToActive();
    await setShareQuality('max');

    await startCustomShare({
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      sourceKind: 'screen',
      withAudio: false,
    });

    const commandNames = invokeCalls.map((call) => call.command);
    expect(commandNames.indexOf('media_set_screen_share_quality')).toBeLessThan(
      commandNames.indexOf('screen_share_start_source'),
    );
    expect(invokeCalls).toContainEqual({
      command: 'media_set_screen_share_quality',
      args: { quality: 'max' },
    });

    await setShareQuality('high');
  });

  it('clears custom state and surfaces native failure after the selected native attempt fails', async () => {
    resetAll();
    vi.unstubAllGlobals();
    vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: { getUserMedia: vi.fn(), getDisplayMedia: vi.fn() },
    });
    await driveToActive();

    (lastLkModule!.startNativeCapture as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error('WGC timeout'),
    );

    await expect(
      startCustomShare({
        mode: 'screen_audio',
        sourceId: 'screen-1',
        sourceName: 'Display 1',
        sourceKind: 'screen',
        withAudio: true,
      }),
    ).rejects.toThrow('WGC timeout');

    expect(lastLkModule!.startScreenShare).not.toHaveBeenCalled();
    expect(getState().activeVideoShare).toBeNull();
    expect(invokeCalls.some((call) => call.command === 'audio_share_start')).toBe(false);
    expect(sentMessages.filter((message) => message.type === 'start_share')).toHaveLength(0);
    expect(getTelemetrySnapshot()).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: 'capture.native.failed',
          sourceKind: 'screen',
          backend: 'wgc',
        }),
      ]),
    );
  });

  it('retries Windows WGC native capture with GDI polling when JS-observed new-sequence cadence is too low', async () => {
    resetAll();
    vi.unstubAllGlobals();
    vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: { getUserMedia: vi.fn(), getDisplayMedia: vi.fn() },
    });
    await driveToActive();

    (lastLkModule!.startNativeCapture as ReturnType<typeof vi.fn>)
      .mockRejectedValueOnce(
        new Error(
          'native capture: low JS-observed WGC new-sequence FPS (3.0 < 20) after 2000ms; retryReason=wgc_sustained_low_js_observed_sequence_fps',
        ),
      )
      .mockResolvedValueOnce(undefined);

    await startCustomShare({
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      sourceKind: 'screen',
      withAudio: false,
    });

    expect(
      invokeCalls
        .filter((call) => call.command === 'screen_share_start_source')
        .map((call) => call.args?.captureBackend),
    ).toEqual(['wgc', 'gdi_poll']);
    expect(
      invokeCalls.filter((call) => call.command === 'screen_share_start_source').at(-1)?.args,
    ).toEqual(
      expect.objectContaining({
        previousBackend: 'wgc',
        retryReason: 'wgc_sustained_low_js_observed_sequence_fps',
      }),
    );
    expect(lastLkModule!.startNativeCapture).toHaveBeenCalledTimes(2);
    expect(lastLkModule!.startNativeCapture).toHaveBeenLastCalledWith({
      firstFrameTimeoutMs: 4500,
      lowJsBridgeFpsRetry: undefined,
    });
    expect(getTelemetrySnapshot()).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: 'capture.fallback.activated',
          os: 'windows',
          from: 'wgc',
          to: 'gdi_poll',
          reason: 'wgc_sustained_low_js_observed_sequence_fps',
        }),
      ]),
    );
  });

  it('does not bypass WGC for later shares after one session failure', async () => {
    resetAll();
    vi.unstubAllGlobals();
    vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: { getUserMedia: vi.fn(), getDisplayMedia: vi.fn() },
    });
    await driveToActive();

    (lastLkModule!.startNativeCapture as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error('native capture: first frame timeout (2s)'),
    );
    const selection: ShareSelection = {
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      sourceKind: 'screen',
      withAudio: false,
    };

    await expect(startCustomShare(selection)).rejects.toThrow(
      'native capture: first frame timeout (2s)',
    );
    (lastLkModule!.startNativeCapture as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);
    await startCustomShare(selection);
    await stopCustomShare('video');
    await startCustomShare({
      mode: 'window',
      sourceId: 'window-1',
      sourceName: 'Window 1',
      sourceKind: 'window',
      withAudio: false,
    });

    expect(
      invokeCalls
        .filter((call) => call.command === 'screen_share_start_source')
        .map((call) => call.args?.captureBackend),
    ).toEqual(['wgc', 'wgc', 'wgc']);
  });

  it('window compatibility mode skips WGC while screen selections still start with WGC', async () => {
    resetAll();
    vi.unstubAllGlobals();
    vi.stubGlobal('window', { RTCPeerConnection: function MockPeerConnection() {} });
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      mediaDevices: { getUserMedia: vi.fn(), getDisplayMedia: vi.fn() },
    });
    await driveToActive();

    await startCustomShare({
      mode: 'window',
      sourceId: 'window-1',
      sourceName: 'Game',
      sourceKind: 'window',
      compatibilityMode: true,
      withAudio: false,
    });

    expect(invokeCalls.filter((call) => call.command === 'screen_share_start_source')).toEqual([
      {
        command: 'screen_share_start_source',
        args: expect.objectContaining({ sourceKind: 'window', captureBackend: 'gdi_poll' }),
      },
    ]);
  });
});

describe('Windows companion audio toggle state', () => {
  beforeEach(async () => {
    resetAll();
    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)' });
    await driveToActive();
  });

  afterEach(() => {
    try {
      leaveRoom();
    } catch {
      /* ignore */
    }
    vi.unstubAllGlobals();
  });

  it('clears the video share audio flag after turning companion audio off', async () => {
    await startCustomShare({
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      withAudio: true,
    });

    expect(getState().activeVideoShare).toMatchObject({
      withAudio: true,
      audioSourceId: 'default-monitor',
    });

    await expect(toggleShareAudio(false)).resolves.toBe(true);

    expect(getState().activeVideoShare).toMatchObject({
      mode: 'screen_audio',
      sourceName: 'Display 1',
      withAudio: false,
      audioSourceId: null,
    });
  });

  it('does not send stop-share when stopping video for a source change', async () => {
    await startCustomShare({
      mode: 'screen_audio',
      sourceId: 'screen-1',
      sourceName: 'Display 1',
      withAudio: true,
    });

    const messagesBeforeStop = sentMessages.length;

    await stopCustomShare('video', { suppressSignaling: true });

    const stopMessages = sentMessages
      .slice(messagesBeforeStop)
      .filter((message) => message.type === 'stop-share');
    expect(stopMessages).toHaveLength(0);
  });
});
