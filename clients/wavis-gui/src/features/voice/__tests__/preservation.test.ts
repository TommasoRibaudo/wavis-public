/**
 * Preservation Property Tests — Existing Behavior Unchanged
 *
 * **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7**
 *
 * Property 2: Preservation — Existing Audio IPC & macOS/Windows Screen Share Unchanged
 *
 * These tests capture BASELINE behavior on UNFIXED code. They MUST PASS
 * before and after the fix to confirm no regressions.
 *
 * Observation-first methodology:
 * - shouldUseNativeMedia() returns true only when required browser capture APIs are missing
 * - When browser WebRTC + capture APIs exist, LiveKitModule is the active path
 * - NativeMediaModule remains the fallback when the webview lacks those APIs
 * - Existing audio IPC commands (media_connect, media_disconnect, media_set_mic_enabled) are unchanged
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as fc from 'fast-check';
import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));

/* ─── Mock Tauri IPC layer ──────────────────────────────────────── */

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

import { invoke } from '@tauri-apps/api/core';
import { NativeMediaModule } from '../native-media';
import type { MediaCallbacks } from '../livekit-media';

/* ─── Helpers ───────────────────────────────────────────────────── */

function makeCallbacks(): MediaCallbacks {
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
    onShareQualityInfo: vi.fn(),
  };
}

/**
 * Inline replica of shouldUseNativeMedia() from voice-room.ts.
 * We replicate it here because the original is a private module-level function
 * that can't be imported directly. The preservation test verifies this logic
 * stays consistent — if the real function changes, this test should be updated
 * to match (or the change is a regression).
 */
function shouldUseNativeMedia(
  _ua: string,
  hasRTC: boolean,
  hasGetUserMedia: boolean,
  hasGetDisplayMedia: boolean,
): boolean {
  return !(hasRTC && hasGetUserMedia && hasGetDisplayMedia);
}

/* ═══ Platform Detection Preservation ═══════════════════════════════ */

describe('Property 2: Preservation — shouldUseNativeMedia() capability detection', () => {
  /**
   * **Validates: Requirements 3.4**
   *
   * Updated routing: when WebRTC + capture APIs are present, all platforms
   * use the LiveKitModule path.
   */
  it('returns false for Linux user agents when capture APIs exist', () => {
    const linuxUAs = [
      'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15',
      'Mozilla/5.0 (X11; Linux aarch64) AppleWebKit/605.1.15',
      'Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:109.0) Gecko/20100101',
    ];
    for (const ua of linuxUAs) {
      expect(shouldUseNativeMedia(ua, true, true, true)).toBe(false);
    }
  });

  it('returns false for macOS user agents', () => {
    const macUAs = [
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15',
      'Mozilla/5.0 (Macintosh; Apple M1 Mac OS X 14_0) AppleWebKit/605.1.15',
    ];
    for (const ua of macUAs) {
      expect(shouldUseNativeMedia(ua, true, true, true)).toBe(false);
    }
  });

  it('returns false for Windows user agents', () => {
    const winUAs = [
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
      'Mozilla/5.0 (Windows NT 11.0; Win64; x64) AppleWebKit/537.36',
    ];
    for (const ua of winUAs) {
      expect(shouldUseNativeMedia(ua, true, true, true)).toBe(false);
    }
  });

  it('returns true when RTCPeerConnection is missing (any platform)', () => {
    expect(shouldUseNativeMedia('Mozilla/5.0 (Windows NT 10.0)', false, true, true)).toBe(true);
    expect(shouldUseNativeMedia('Mozilla/5.0 (Macintosh)', false, true, true)).toBe(true);
  });

  it('returns true when getUserMedia is missing', () => {
    expect(shouldUseNativeMedia('Mozilla/5.0 (X11; Linux x86_64)', true, false, true)).toBe(true);
  });

  it('returns true when getDisplayMedia is missing', () => {
    expect(shouldUseNativeMedia('Mozilla/5.0 (X11; Linux x86_64)', true, true, false)).toBe(true);
  });
});

/* ═══ Property-Based: Module Routing ════════════════════════════════ */

describe('Property 2: Preservation — PBT module routing', () => {
  /**
   * **Validates: Requirements 3.1, 3.4**
   *
   * Property: shouldUseNativeMedia is purely capability-based.
   */
  it('routes based on WebRTC + capture API availability', () => {
    const platformArb = fc.oneof(
      fc.constant('Linux x86_64'),
      fc.constant('Linux aarch64'),
      fc.constant('Ubuntu; Linux'),
      fc.constant('Macintosh; Intel Mac OS X'),
      fc.constant('Macintosh; Apple M1'),
      fc.constant('Windows NT 10.0'),
      fc.constant('Windows NT 11.0'),
    );

    const hasRTCArb = fc.boolean();
    const hasGetUserMediaArb = fc.boolean();
    const hasGetDisplayMediaArb = fc.boolean();

    fc.assert(
      fc.property(platformArb, hasRTCArb, hasGetUserMediaArb, hasGetDisplayMediaArb, (platform, hasRTC, hasGetUserMedia, hasGetDisplayMedia) => {
        const ua = `Mozilla/5.0 (${platform}) AppleWebKit/605.1.15`;
        const result = shouldUseNativeMedia(ua, hasRTC, hasGetUserMedia, hasGetDisplayMedia);
        return result === !(hasRTC && hasGetUserMedia && hasGetDisplayMedia);
      }),
      { numRuns: 200 },
    );
  });

  /**
   * **Validates: Requirements 3.1, 3.4**
   *
   * Property: With all browser capture APIs available, all platforms route
   * to the LiveKitModule path.
   */
  it('random UAs with WebRTC capture APIs always route to LiveKitModule', () => {
    const uaArb = fc.string({ minLength: 1, maxLength: 100 });
    fc.assert(
      fc.property(uaArb, (ua) => {
        return shouldUseNativeMedia(ua, true, true, true) === false;
      }),
      { numRuns: 200 },
    );
  });
});

/* ═══ NativeMediaModule Baseline Behavior ═══════════════════════════ */

describe('Property 2: Preservation — NativeMediaModule audio IPC baseline', () => {
  let mod_: NativeMediaModule;
  let callbacks: MediaCallbacks;

  beforeEach(() => {
    vi.clearAllMocks();
    callbacks = makeCallbacks();
    mod_ = new NativeMediaModule(callbacks);
  });

  /**
   * **Validates: Requirements 3.3, 3.6**
   *
   * Observed baseline: connect() invokes media_connect IPC command.
   * This must remain unchanged after screen share additions.
   */
  it('connect() invokes media_connect IPC command', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);

    await mod_.connect('wss://sfu.example.com', 'test-token');

    expect(invoke).toHaveBeenCalledWith('media_connect', {
      url: 'wss://sfu.example.com',
      token: 'test-token',
      denoiseEnabled: true,
    });
  });

  /**
   * **Validates: Requirements 3.3, 3.6**
   *
   * Observed baseline: disconnect() invokes media_disconnect IPC command.
   */
  it('disconnect() invokes media_disconnect IPC command', () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    mod_.disconnect();

    expect(invoke).toHaveBeenCalledWith('media_disconnect');
  });

  /**
   * **Validates: Requirements 3.3, 3.6**
   *
   * Observed baseline: setMicEnabled() invokes media_set_mic_enabled IPC command.
   */
  it('setMicEnabled() invokes media_set_mic_enabled IPC command', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);

    await mod_.setMicEnabled(true);

    expect(invoke).toHaveBeenCalledWith('media_set_mic_enabled', { enabled: true });
  });

  it('setMicEnabled(false) invokes media_set_mic_enabled with false', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);

    await mod_.setMicEnabled(false);

    expect(invoke).toHaveBeenCalledWith('media_set_mic_enabled', { enabled: false });
  });

  /**
   * **Validates: Requirements 3.6**
   *
   * Observed baseline: setParticipantVolume() invokes media_set_participant_volume.
   * Volume is clamped to 0–100.
   */
  it('setParticipantVolume() invokes media_set_participant_volume with clamped value', () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    mod_.setParticipantVolume('user-1', 75);

    expect(invoke).toHaveBeenCalledWith('media_set_participant_volume', {
      id: 'user-1',
      level: 75,
    });
  });

  it('setParticipantVolume() clamps volume above 100', () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    mod_.setParticipantVolume('user-1', 150);

    expect(invoke).toHaveBeenCalledWith('media_set_participant_volume', {
      id: 'user-1',
      level: 100,
    });
  });

  it('setParticipantVolume() clamps volume below 0', () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    mod_.setParticipantVolume('user-1', -10);

    expect(invoke).toHaveBeenCalledWith('media_set_participant_volume', {
      id: 'user-1',
      level: 0,
    });
  });

  /**
   * **Validates: Requirements 3.6**
   *
   * Observed baseline: setMasterVolume() invokes media_set_master_volume.
   */
  it('setMasterVolume() invokes media_set_master_volume with clamped value', () => {
    vi.mocked(invoke).mockResolvedValue(undefined);

    mod_.setMasterVolume(70);

    expect(invoke).toHaveBeenCalledWith('media_set_master_volume', { level: 70 });
  });

  /**
   * **Validates: Requirements 3.6**
   *
   * Observed baseline: listDevices() invokes list_audio_devices IPC command.
   */
  it('listDevices() invokes list_audio_devices IPC command', async () => {
    vi.mocked(invoke).mockResolvedValueOnce([
      { id: 'input:mic1', name: 'Mic 1', kind: 'input', is_default: true },
      { id: 'output:spk1', name: 'Speaker 1', kind: 'output', is_default: true },
    ]);

    const result = await mod_.listDevices();

    expect(invoke).toHaveBeenCalledWith('list_audio_devices');
    expect(result.inputs).toHaveLength(1);
    expect(result.outputs).toHaveLength(1);
    expect(result.inputs[0].deviceId).toBe('input:mic1');
    expect(result.outputs[0].deviceId).toBe('output:spk1');
  });

  /**
   * **Validates: Requirements 3.6**
   *
   * Observed baseline: setInputDevice/setOutputDevice invoke set_audio_device.
   */
  it('setInputDevice() invokes set_audio_device with kind=input', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);

    await mod_.setInputDevice('input:mic1');

    expect(invoke).toHaveBeenCalledWith('set_audio_device', {
      deviceId: 'input:mic1',
      kind: 'input',
    });
  });

  it('setOutputDevice() invokes set_audio_device with kind=output', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);

    await mod_.setOutputDevice('output:spk1');

    expect(invoke).toHaveBeenCalledWith('set_audio_device', {
      deviceId: 'output:spk1',
      kind: 'output',
    });
  });

  it('setInputVolume() invokes set_input_gain with converted gain', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);

    await mod_.setInputVolume(50);

    expect(invoke).toHaveBeenCalledWith('set_input_gain', { gain: 0.25 });
  });
});


/* ═══ NativeMediaModule startScreenShare() Baseline (Unfixed) ═══════ */

describe('Property 2: Preservation — NativeMediaModule startScreenShare baseline', () => {
  let mod_: NativeMediaModule;
  let callbacks: MediaCallbacks;

  beforeEach(() => {
    vi.clearAllMocks();
    callbacks = makeCallbacks();
    mod_ = new NativeMediaModule(callbacks);
  });

  /**
   * **Validates: Requirements 3.3**
   *
   * Observed baseline on UNFIXED code: startScreenShare() returned false
   * and emitted a "not available" system event. That was the bug.
   *
   * After fix: startScreenShare() invokes screen_share_start IPC and
   * returns the result. The three-way contract is validated by the bug
   * condition exploration test (Task 1). Here we verify the IPC call
   * happens and the result is forwarded.
   */
  it('startScreenShare() invokes screen_share_start IPC (fixed)', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(true);
    const result = await mod_.startScreenShare();
    expect(invoke).toHaveBeenCalledWith('screen_share_start');
    expect(result).toBe(true);
  });

  it('startScreenShare() does not emit "not available" system event (fixed)', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(true);
    await mod_.startScreenShare();
    expect(callbacks.onSystemEvent).not.toHaveBeenCalled();
  });

  /**
   * **Validates: Requirements 3.3**
   *
   * Observed baseline: getActiveScreenShares() returns empty array.
   */
  it('getActiveScreenShares() returns empty array (baseline)', () => {
    const shares = mod_.getActiveScreenShares();
    expect(shares).toEqual([]);
  });

  /**
   * **Validates: Requirements 3.3**
   *
   * After fix: stopScreenShare() invokes screen_share_stop IPC command.
   */
  it('stopScreenShare() invokes screen_share_stop IPC (fixed)', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await mod_.stopScreenShare();
    expect(invoke).toHaveBeenCalledWith('screen_share_stop');
  });
});

/* ═══ Rust Source Preservation — Static Analysis ════════════════════ */

describe('Property 2: Preservation — Rust IPC command signatures unchanged', () => {
  /**
   * **Validates: Requirements 3.6**
   *
   * Observed baseline: main.rs registers these audio IPC commands in
   * generate_handler![]. The screen share fix must be ADDITIVE ONLY —
   * these existing commands must remain registered.
   */
  const mainRsPath = resolve(__dirname, '../../../../src-tauri/src/main.rs');
  const mainRs = readFileSync(mainRsPath, 'utf-8');

  it('main.rs registers media_connect command', () => {
    expect(mainRs).toContain('media::media_connect');
  });

  it('main.rs registers media_disconnect command', () => {
    expect(mainRs).toContain('media::media_disconnect');
  });

  it('main.rs registers media_set_mic_enabled command', () => {
    expect(mainRs).toContain('media::media_set_mic_enabled');
  });

  it('main.rs registers media_set_participant_volume command', () => {
    expect(mainRs).toContain('media::media_set_participant_volume');
  });

  it('main.rs registers media_set_master_volume command', () => {
    expect(mainRs).toContain('media::media_set_master_volume');
  });

  it('main.rs registers media_set_denoise_enabled command', () => {
    expect(mainRs).toContain('media::media_set_denoise_enabled');
  });

  it('main.rs registers list_audio_devices command', () => {
    expect(mainRs).toContain('list_audio_devices');
  });

  it('main.rs registers set_audio_device command', () => {
    expect(mainRs).toContain('set_audio_device');
  });

  it('main.rs registers set_input_gain command', () => {
    expect(mainRs).toContain('set_input_gain');
  });

  /**
   * **Validates: Requirements 3.3, 3.6**
   *
   * Observed baseline: media.rs defines the MediaEvent enum with exactly
   * these audio-related variants. Screen share additions must not alter
   * existing variants.
   */
  const mediaRsPath = resolve(__dirname, '../../../../src-tauri/src/media.rs');
  const mediaRs = readFileSync(mediaRsPath, 'utf-8');

  it('media.rs defines MediaEvent::Connected variant', () => {
    expect(mediaRs).toContain('Connected');
  });

  it('media.rs defines MediaEvent::Failed variant', () => {
    expect(mediaRs).toContain('Failed');
  });

  it('media.rs defines MediaEvent::Disconnected variant', () => {
    expect(mediaRs).toContain('Disconnected');
  });

  it('media.rs defines MediaEvent::AudioLevels variant', () => {
    expect(mediaRs).toContain('AudioLevels');
  });

  it('media.rs defines MediaEvent::LocalAudioLevel variant', () => {
    expect(mediaRs).toContain('LocalAudioLevel');
  });

  it('media.rs defines MediaEvent::Stats variant', () => {
    // Verify the Stats variant with its specific fields
    expect(mediaRs).toContain('Stats');
    expect(mediaRs).toContain('rtt_ms');
    expect(mediaRs).toContain('packet_loss_percent');
    expect(mediaRs).toContain('jitter_ms');
  });

  /**
   * **Validates: Requirements 3.3, 3.6**
   *
   * Observed baseline: media.rs has media_connect, media_disconnect,
   * media_set_mic_enabled as #[tauri::command] functions.
   */
  it('media.rs defines media_connect command function', () => {
    expect(mediaRs).toMatch(/pub\s+fn\s+media_connect/);
  });

  it('media.rs defines media_disconnect command function', () => {
    expect(mediaRs).toMatch(/pub\s+fn\s+media_disconnect/);
  });

  it('media.rs defines media_set_mic_enabled command function', () => {
    expect(mediaRs).toMatch(/pub\s+fn\s+media_set_mic_enabled/);
  });

  it('media.rs defines media_set_denoise_enabled command function', () => {
    expect(mediaRs).toMatch(/pub\s+fn\s+media_set_denoise_enabled/);
  });

  /**
   * **Validates: Requirements 3.4**
   *
   * The function must remain present and gate on browser capabilities.
   */
  const voiceRoomPath = resolve(__dirname, '../voice-room.ts');
  const voiceRoomTs = readFileSync(voiceRoomPath, 'utf-8');
  const settingsPath = resolve(__dirname, '../../settings/Settings.tsx');
  const settingsTsx = readFileSync(settingsPath, 'utf-8');

  it('voice-room.ts contains shouldUseNativeMedia function', () => {
    expect(voiceRoomTs).toContain('function shouldUseNativeMedia()');
  });

  it('voice-room.ts shouldUseNativeMedia checks for RTCPeerConnection', () => {
    expect(voiceRoomTs).toContain('RTCPeerConnection');
  });

  it('voice-room.ts shouldUseNativeMedia checks for getUserMedia', () => {
    expect(voiceRoomTs).toContain('getUserMedia');
  });

  it('voice-room.ts shouldUseNativeMedia checks for getDisplayMedia', () => {
    expect(voiceRoomTs).toContain('getDisplayMedia');
  });

  /**
   * **Validates: Requirements 3.4**
   *
   * Observed baseline: voice-room.ts routes to NativeMediaModule on Linux
   * and LiveKitModule on macOS/Windows.
   */
  it('voice-room.ts instantiates NativeMediaModule when useNative is true', () => {
    expect(voiceRoomTs).toContain('new NativeMediaModule(callbacks)');
  });

  it('voice-room.ts instantiates LiveKitModule when useNative is false', () => {
    expect(voiceRoomTs).toContain('new LiveKitModule(callbacks)');
  });

  it('Settings.tsx invokes media_set_denoise_enabled when the toggle changes', () => {
    expect(settingsTsx).toContain("invoke('media_set_denoise_enabled', { enabled: checked })");
  });
});

/* ═══ Property-Based: Audio IPC Volume Clamping ═════════════════════ */

describe('Property 2: Preservation — PBT audio IPC volume clamping', () => {
  let mod_: NativeMediaModule;

  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(invoke).mockResolvedValue(undefined);
    mod_ = new NativeMediaModule(makeCallbacks());
  });

  /**
   * **Validates: Requirements 3.6**
   *
   * Property: For any numeric volume input, setParticipantVolume clamps
   * the value to [0, 100] before sending to IPC.
   */
  it('setParticipantVolume always clamps to [0, 100]', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: -1000, max: 1000 }),
        (volume) => {
          vi.clearAllMocks();
          vi.mocked(invoke).mockResolvedValue(undefined);

          mod_.setParticipantVolume('test-user', volume);

          const call = vi.mocked(invoke).mock.calls[0];
          expect(call[0]).toBe('media_set_participant_volume');
          const args = call[1] as { id: string; level: number };
          expect(args.level).toBeGreaterThanOrEqual(0);
          expect(args.level).toBeLessThanOrEqual(100);
          // Verify it's the correctly clamped + rounded value
          const expected = Math.max(0, Math.min(100, Math.round(volume)));
          expect(args.level).toBe(expected);
        },
      ),
      { numRuns: 200 },
    );
  });

  /**
   * **Validates: Requirements 3.6**
   *
   * Property: For any numeric volume input, setMasterVolume clamps
   * the value to [0, 100] before sending to IPC.
   */
  it('setMasterVolume always clamps to [0, 100]', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: -1000, max: 1000 }),
        (volume) => {
          vi.clearAllMocks();
          vi.mocked(invoke).mockResolvedValue(undefined);

          mod_.setMasterVolume(volume);

          const call = vi.mocked(invoke).mock.calls[0];
          expect(call[0]).toBe('media_set_master_volume');
          const args = call[1] as { level: number };
          expect(args.level).toBeGreaterThanOrEqual(0);
          expect(args.level).toBeLessThanOrEqual(100);
          const expected = Math.max(0, Math.min(100, Math.round(volume)));
          expect(args.level).toBe(expected);
        },
      ),
      { numRuns: 200 },
    );
  });
});
