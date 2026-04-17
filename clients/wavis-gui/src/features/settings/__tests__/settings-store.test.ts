/**
 * Property 4: Settings store round-trip
 *
 * For any settings key and valid value, setStoreValue(key, value) then
 * getStoreValue(key, default) returns the original value.
 *
 * Validates: Requirements 9.3, 10.5, 12.3, 13.3, 14.2, 14.4, 15.6, 16.3, 11.6
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import fc from 'fast-check';

/* ─── Mock @tauri-apps/plugin-store ─────────────────────────────── */

const mockStorage = new Map<string, unknown>();

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

/* ─── Import after mock ─────────────────────────────────────────── */

import {
  getStoreValue,
  setStoreValue,
  getProfileColor,
  setProfileColor,
  getDefaultVolume,
  setDefaultVolume,
  getNotificationToggles,
  setNotificationToggle,
  isNotificationEnabled,
  getMinimizeToTray,
  setMinimizeToTray,
  getReconnectConfig,
  setReconnectConfig,
  getMuteHotkey,
  setMuteHotkey,
  getDenoiseEnabled,
  setDenoiseEnabled,
  STORE_KEYS,
  DEFAULT_VOLUME,
  DEFAULT_MUTE_HOTKEY,
  DEFAULT_RECONNECT_CONFIG,
} from '../settings-store';
import { PROFILE_COLORS } from '@shared/colors';
import type { ReconnectConfig, NotificationToggles } from '../settings-store';

beforeEach(() => {
  mockStorage.clear();
});

/* ─── Arbitraries ───────────────────────────────────────────────── */

/**
 * JSON-serializable non-null values. The store uses `value ?? defaultValue`
 * (nullish coalescing), so null is intentionally treated as "absent" — this
 * matches the design: a missing key returns the default.
 */
const jsonValue = fc.oneof(
  fc.string(),
  fc.integer(),
  fc.double({ noNaN: true, noDefaultInfinity: true }),
  fc.boolean(),
);

const storeKey = fc.constantFrom(...Object.values(STORE_KEYS));

const reconnectConfigArb: fc.Arbitrary<ReconnectConfig> = fc.record({
  strategy: fc.constantFrom('exponential' as const, 'fixed' as const),
  baseDelayMs: fc.integer({ min: 100, max: 60000 }),
  maxDelayMs: fc.integer({ min: 1000, max: 120000 }),
  maxRetries: fc.integer({ min: 1, max: 100 }),
});

const notificationEventArb = fc.constantFrom<(keyof NotificationToggles)[]>(
  'participantJoined',
  'participantLeft',
  'participantKicked',
  'participantMutedByHost',
  'inviteReceived',
);

/* ═══ Property Tests ════════════════════════════════════════════════ */

describe('Property 4: Settings store round-trip', () => {
  // Feature: gui-feature-completion, Property 4: Settings store round-trip
  // **Validates: Requirements 9.3, 10.5, 12.3, 13.3, 14.2, 14.4, 15.6, 16.3, 11.6**

  it('setStoreValue then getStoreValue returns the original value', async () => {
    await fc.assert(
      fc.asyncProperty(storeKey, jsonValue, async (key, value) => {
        mockStorage.clear();
        await setStoreValue(key, value);
        const sentinel = Symbol('sentinel');
        const result = await getStoreValue(key, sentinel as unknown);
        expect(result).toEqual(value);
      }),
      { numRuns: 100 },
    );
  });

  it('getStoreValue returns default when key is absent', async () => {
    await fc.assert(
      fc.asyncProperty(storeKey, jsonValue, async (key, defaultValue) => {
        mockStorage.clear();
        const result = await getStoreValue(key, defaultValue);
        expect(result).toEqual(defaultValue);
      }),
      { numRuns: 100 },
    );
  });

  it('round-trip with complex objects (ReconnectConfig)', async () => {
    await fc.assert(
      fc.asyncProperty(reconnectConfigArb, async (config) => {
        mockStorage.clear();
        await setReconnectConfig(config);
        const result = await getReconnectConfig();
        expect(result).toEqual(config);
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Typed Convenience Function Tests ══════════════════════════════ */

describe('Profile color round-trip', () => {
  it('persists and retrieves any profile color', async () => {
    const colorArb = fc.constantFrom(...PROFILE_COLORS);
    await fc.assert(
      fc.asyncProperty(colorArb, async (color) => {
        mockStorage.clear();
        await setProfileColor(color);
        const result = await getProfileColor();
        expect(result).toBe(color);
      }),
      { numRuns: 100 },
    );
  });

  it('defaults to first color when unset', async () => {
    const result = await getProfileColor();
    expect(result).toBe(PROFILE_COLORS[0]);
  });
});

describe('Default volume round-trip', () => {
  it('persists and retrieves any volume 0–100', async () => {
    const volumeArb = fc.integer({ min: 0, max: 100 });
    await fc.assert(
      fc.asyncProperty(volumeArb, async (volume) => {
        mockStorage.clear();
        await setDefaultVolume(volume);
        const result = await getDefaultVolume();
        expect(result).toBe(volume);
      }),
      { numRuns: 100 },
    );
  });

  it('defaults to DEFAULT_VOLUME when unset', async () => {
    const result = await getDefaultVolume();
    expect(result).toBe(DEFAULT_VOLUME);
  });
});

describe('Notification toggle round-trip', () => {
  it('setNotificationToggle then isNotificationEnabled returns the set value', async () => {
    await fc.assert(
      fc.asyncProperty(notificationEventArb, fc.boolean(), async (event, enabled) => {
        mockStorage.clear();
        await setNotificationToggle(event, enabled);
        const result = await isNotificationEnabled(event);
        expect(result).toBe(enabled);
      }),
      { numRuns: 100 },
    );
  });

  it('all toggles default to true when unset', async () => {
    const toggles = await getNotificationToggles();
    expect(toggles).toEqual({
      participantJoined: true,
      participantLeft: true,
      participantKicked: true,
      participantMutedByHost: true,
      inviteReceived: true,
    });
  });
});

describe('Minimize to tray round-trip', () => {
  it('persists and retrieves boolean', async () => {
    await fc.assert(
      fc.asyncProperty(fc.boolean(), async (enabled) => {
        mockStorage.clear();
        await setMinimizeToTray(enabled);
        const result = await getMinimizeToTray();
        expect(result).toBe(enabled);
      }),
      { numRuns: 100 },
    );
  });

  it('defaults to false when unset', async () => {
    const result = await getMinimizeToTray();
    expect(result).toBe(false);
  });
});

describe('Mute hotkey round-trip', () => {
  it('persists and retrieves any hotkey string', async () => {
    const hotkeyParts = ['Ctrl', 'Shift', 'Alt', 'Meta', 'A', 'B', 'M', 'F1', 'Space'];
    const hotkeyArb = fc
      .subarray(hotkeyParts, { minLength: 1, maxLength: 4 })
      .map((parts) => parts.join('+'));
    await fc.assert(
      fc.asyncProperty(hotkeyArb, async (hotkey) => {
        mockStorage.clear();
        await setMuteHotkey(hotkey);
        const result = await getMuteHotkey();
        expect(result).toBe(hotkey);
      }),
      { numRuns: 100 },
    );
  });

  it('defaults to DEFAULT_MUTE_HOTKEY when unset', async () => {
    const result = await getMuteHotkey();
    expect(result).toBe(DEFAULT_MUTE_HOTKEY);
  });
});

describe('Reconnect config round-trip', () => {
  it('defaults to DEFAULT_RECONNECT_CONFIG when unset', async () => {
    const result = await getReconnectConfig();
    expect(result).toEqual(DEFAULT_RECONNECT_CONFIG);
  });
});

describe('Denoise setting round-trip', () => {
  it('persists and retrieves boolean', async () => {
    await fc.assert(
      fc.asyncProperty(fc.boolean(), async (enabled) => {
        mockStorage.clear();
        await setDenoiseEnabled(enabled);
        const result = await getDenoiseEnabled();
        expect(result).toBe(enabled);
      }),
      { numRuns: 100 },
    );
  });

  it('defaults to true when unset', async () => {
    const result = await getDenoiseEnabled();
    expect(result).toBe(true);
  });
});

/* ═══ Watch All Hotkey Tests ════════════════════════════════════════ */

import {
  getWatchAllHotkey,
  setWatchAllHotkey,
  DEFAULT_WATCH_ALL_HOTKEY,
  inputVolumeToGain,
} from '../settings-store';

/* ═══ inputVolumeToGain ═════════════════════════════════════════════ */

describe('inputVolumeToGain', () => {
  it('maps 0 to 0 (muted) and 100 to 1 (unity)', () => {
    expect(inputVolumeToGain(0)).toBe(0);
    expect(inputVolumeToGain(100)).toBe(1);
  });

  it('maps 50 to 0.25 (square law: half perceived loudness)', () => {
    expect(inputVolumeToGain(50)).toBeCloseTo(0.25, 10);
  });

  it('output is always in [0, 1] for any slider value', () => {
    fc.assert(
      fc.property(fc.integer({ min: 0, max: 100 }), (v) => {
        const g = inputVolumeToGain(v);
        expect(g).toBeGreaterThanOrEqual(0);
        expect(g).toBeLessThanOrEqual(1);
      }),
      { numRuns: 101 },
    );
  });

  it('is monotonically non-decreasing across the full range', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: 99 }),
        fc.integer({ min: 1, max: 100 }),
        (a, b) => {
          fc.pre(a < b);
          expect(inputVolumeToGain(a)).toBeLessThanOrEqual(inputVolumeToGain(b));
        },
      ),
      { numRuns: 200 },
    );
  });

  it('clamps out-of-range inputs', () => {
    expect(inputVolumeToGain(-10)).toBe(0);
    expect(inputVolumeToGain(200)).toBe(1);
  });

  it('satisfies the square law: gain === (volume/100)^2', () => {
    fc.assert(
      fc.property(fc.integer({ min: 0, max: 100 }), (v) => {
        const expected = (v / 100) ** 2;
        expect(inputVolumeToGain(v)).toBeCloseTo(expected, 10);
      }),
      { numRuns: 101 },
    );
  });
});

describe('Watch All hotkey round-trip', () => {
  it('defaults to DEFAULT_WATCH_ALL_HOTKEY when unset', async () => {
    const result = await getWatchAllHotkey();
    expect(result).toBe(DEFAULT_WATCH_ALL_HOTKEY);
    expect(result).toBe('CmdOrCtrl+Shift+W');
  });

  it('persists and retrieves any hotkey string', async () => {
    const hotkeyParts = ['Ctrl', 'Shift', 'Alt', 'Meta', 'W', 'A', 'F1', 'Space'];
    const hotkeyArb = fc
      .subarray(hotkeyParts, { minLength: 1, maxLength: 4 })
      .map((parts) => parts.join('+'));
    await fc.assert(
      fc.asyncProperty(hotkeyArb, async (hotkey) => {
        mockStorage.clear();
        await setWatchAllHotkey(hotkey);
        const result = await getWatchAllHotkey();
        expect(result).toBe(hotkey);
      }),
      { numRuns: 100 },
    );
  });
});
