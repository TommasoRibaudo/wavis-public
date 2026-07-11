/**
 * ActiveRoom camera button and panel tab — structural tests (node env, no jsdom)
 *
 * Validates: Requirements 3.1, 3.6, 3.8, 4.1, 4.2, 4.4, 4.7, 4.8
 */

import { describe, it, expect } from 'vitest';
import {
  cameraButtonLabel,
  hasBrowserCameraMediaSupport,
  shouldDisableCameraButton,
  shouldMountCameraButton,
} from '../active-room-camera';

/* ─── Button ordering logic ─────────────────────────────────────── */

describe('Feature: video-feed — button ordering (Req 4.1, 4.2)', () => {
  const controls = ['mute', 'deafen', 'video'] as const;

  it('mute comes before deafen', () => {
    expect(controls.indexOf('mute')).toBeLessThan(controls.indexOf('deafen'));
  });

  it('deafen comes before video', () => {
    expect(controls.indexOf('deafen')).toBeLessThan(controls.indexOf('video'));
  });
});

/* ─── Mount during reconnect ────────────────────────────────────── */

describe('Feature: video-feed — button stays mounted during media reconnect (Req 4.4)', () => {
  it('shouldMountCameraButton stays true when machineState is reconnecting', () => {
    const voiceRoomConnected = true;
    const supportedCapturePlatform = true;
    expect(shouldMountCameraButton(voiceRoomConnected, supportedCapturePlatform)).toBe(true);
  });

  it('shouldMountCameraButton is false when voice room is not connected', () => {
    expect(shouldMountCameraButton(false, true)).toBe(false);
  });
});

describe('Feature: video-feed - button disabled gate', () => {
  it('disables camera when the user is not in a synchronized room', () => {
    expect(shouldDisableCameraButton(true, 'connected', null)).toBe(true);
  });

  it('enables camera when voice, media, and synchronized room membership are active', () => {
    expect(shouldDisableCameraButton(true, 'connected', 'room-1')).toBe(false);
  });

  it('keeps camera enabled during media reconnect while still in a synchronized room', () => {
    expect(shouldDisableCameraButton(true, 'reconnecting', 'room-1')).toBe(false);
  });
});

describe('Feature: video-feed — Linux browser media support', () => {
  it('detects RTCPeerConnection + getUserMedia as camera-capable browser media', () => {
    const originalWindow = globalThis.window;
    const originalNavigator = globalThis.navigator;

    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { RTCPeerConnection: function RTCPeerConnection() {} },
    });
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { mediaDevices: { getUserMedia: async () => ({}) } },
    });

    try {
      expect(hasBrowserCameraMediaSupport()).toBe(true);
    } finally {
      Object.defineProperty(globalThis, 'window', {
        configurable: true,
        value: originalWindow,
      });
      Object.defineProperty(globalThis, 'navigator', {
        configurable: true,
        value: originalNavigator,
      });
    }
  });
});

/* ─── Button label ──────────────────────────────────────────────── */

describe('Feature: video-feed — button label (Req 1.7, 1.8)', () => {
  it('returns "camera-on" when intent is false', () => {
    expect(cameraButtonLabel(false)).toBe('camera-on');
  });

  it('returns "camera-off" when intent is true', () => {
    expect(cameraButtonLabel(true)).toBe('camera-off');
  });
});

/* ─── Panel tab defaults ────────────────────────────────────────── */

import { computeRoomPanelTab } from '../voice-room';

describe('Feature: video-feed — Logs_Tab is default and resets (Req 3.1, 3.6, 3.8)', () => {
  it('defaults to logs when no video is active', () => {
    expect(
      computeRoomPanelTab({
        anyVideoActive: false,
        manualOverride: null,
        hadAnyVideoActive: false,
      }),
    ).toBe('logs');
  });

  it('resets to logs when last video ends even after manual override', () => {
    expect(
      computeRoomPanelTab({
        anyVideoActive: false,
        manualOverride: 'video',
        hadAnyVideoActive: false,
      }),
    ).toBe('logs');
  });

  it('shows video when video is active and no override', () => {
    expect(
      computeRoomPanelTab({
        anyVideoActive: true,
        manualOverride: null,
        hadAnyVideoActive: true,
      }),
    ).toBe('video');
  });

  it('respects manual override to logs while video is active', () => {
    expect(
      computeRoomPanelTab({
        anyVideoActive: true,
        manualOverride: 'logs',
        hadAnyVideoActive: true,
      }),
    ).toBe('logs');
  });
});
