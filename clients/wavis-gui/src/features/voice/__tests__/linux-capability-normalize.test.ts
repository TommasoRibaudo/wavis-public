import { describe, it, expect } from 'vitest';
import {
  buildUnknownLinuxCapabilityRow,
  normalizeLinuxCompositor,
  normalizeLinuxDesktopEnv,
} from '../linux-capability-normalize';

describe('normalizeLinuxDesktopEnv', () => {
  it('maps known desktop tokens to matrix keys', () => {
    expect(normalizeLinuxDesktopEnv('GNOME')).toBe('gnome');
    expect(normalizeLinuxDesktopEnv('ubuntu:GNOME')).toBe('gnome');
    expect(normalizeLinuxDesktopEnv('KDE')).toBe('kde');
    expect(normalizeLinuxDesktopEnv('plasma')).toBe('kde');
    expect(normalizeLinuxDesktopEnv('sway')).toBe('sway');
    expect(normalizeLinuxDesktopEnv('XFCE')).toBe('xfce');
  });

  it('scans colon-separated lists for a known token', () => {
    expect(normalizeLinuxDesktopEnv('X-Cinnamon:kde-plasma')).toBe('kde');
  });

  it('falls back to the first token for unknown desktops', () => {
    expect(normalizeLinuxDesktopEnv('Budgie:GTK')).toBe('budgie');
  });

  it('returns unknown for empty input', () => {
    expect(normalizeLinuxDesktopEnv('')).toBe('unknown');
    expect(normalizeLinuxDesktopEnv(' : ')).toBe('unknown');
  });
});

describe('normalizeLinuxCompositor', () => {
  it('accepts wayland and x11 case-insensitively', () => {
    expect(normalizeLinuxCompositor('wayland')).toBe('wayland');
    expect(normalizeLinuxCompositor(' X11 ')).toBe('x11');
  });

  it('returns null for anything else', () => {
    expect(normalizeLinuxCompositor('tty')).toBeNull();
    expect(normalizeLinuxCompositor('')).toBeNull();
    expect(normalizeLinuxCompositor('mir')).toBeNull();
  });
});

describe('buildUnknownLinuxCapabilityRow', () => {
  it('produces a degraded x11 row carrying the reason', () => {
    const row = buildUnknownLinuxCapabilityRow('reason text');
    expect(row).toEqual({
      desktopEnv: 'unknown',
      compositor: 'x11',
      videoCapture: 'unsupported',
      audioCapture: 'unsupported',
      combinedStatus: 'degraded',
      userMessage: 'reason text',
    });
  });
});
