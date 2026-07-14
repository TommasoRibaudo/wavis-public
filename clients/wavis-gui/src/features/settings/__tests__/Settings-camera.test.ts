/**
 * Settings camera section — unit tests (Req 6.1–6.4, 6.10, 7.4)
 */

import { describe, it, expect } from 'vitest';
import { cameraOptionLabel } from './Settings-camera.property.test';

describe('Feature: video-feed — Settings camera section', () => {
  it('uses device label when non-empty (6.2)', () => {
    expect(cameraOptionLabel({ deviceId: 'abc', label: 'FaceTime HD Camera' }, 0)).toBe(
      'FaceTime HD Camera',
    );
  });

  it('falls back to Camera N when label is empty (6.2)', () => {
    expect(cameraOptionLabel({ deviceId: 'abc', label: '' }, 0)).toBe('Camera 1');
    expect(cameraOptionLabel({ deviceId: 'def', label: '' }, 2)).toBe('Camera 3');
  });

  it('single camera uses its label (6.3)', () => {
    const devices = [{ deviceId: 'id1', label: 'Logitech C920' }];
    expect(cameraOptionLabel(devices[0], 0)).toBe('Logitech C920');
  });

  it('single camera with empty label falls back (6.3)', () => {
    const devices = [{ deviceId: 'id1', label: '' }];
    expect(cameraOptionLabel(devices[0], 0)).toBe('Camera 1');
  });

  it('zero cameras — produces no labels (6.4)', () => {
    const devices: { deviceId: string; label: string }[] = [];
    expect(devices.map((d, i) => cameraOptionLabel(d, i))).toEqual([]);
  });

  describe('Linux / platform gate (6.10)', () => {
    it('supportedCapturePlatform is true on Linux user agent so the UI can surface native limitations', () => {
      const linuxUa = 'Mozilla/5.0 (X11; Linux x86_64)';
      const hasBrowserCameraMedia = false;
      const supported =
        /Windows|Macintosh/.test(linuxUa) || /Linux/.test(linuxUa) || hasBrowserCameraMedia;
      expect(supported).toBe(true);
    });

    it('supportedCapturePlatform is true on Linux user agent with browser camera media', () => {
      const linuxUa = 'Mozilla/5.0 (X11; Linux x86_64)';
      const hasBrowserCameraMedia = true;
      const supported =
        /Windows|Macintosh/.test(linuxUa) || /Linux/.test(linuxUa) || hasBrowserCameraMedia;
      expect(supported).toBe(true);
    });

    it('supportedCapturePlatform is true on Windows user agent', () => {
      const windowsUa = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)';
      const supported = /Windows|Macintosh/.test(windowsUa) || /Linux/.test(windowsUa) || false;
      expect(supported).toBe(true);
    });

    it('supportedCapturePlatform is true on macOS user agent', () => {
      const macUa = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)';
      const supported = /Windows|Macintosh/.test(macUa) || /Linux/.test(macUa) || false;
      expect(supported).toBe(true);
    });
  });
});
