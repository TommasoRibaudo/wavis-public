import { describe, it, expect, vi, beforeEach } from 'vitest';
import fc from 'fast-check';

/* ─── Mock @tauri-apps/plugin-global-shortcut ───────────────────── */

vi.mock('@tauri-apps/plugin-global-shortcut', () => ({
  register: vi.fn(),
  unregister: vi.fn(),
  isRegistered: vi.fn().mockResolvedValue(false),
}));

import { register, unregister } from '@tauri-apps/plugin-global-shortcut';
import {
  formatHotkeyCombination,
  registerWatchAllHotkey,
  unregisterWatchAllHotkey,
} from '../hotkey-bridge';

beforeEach(() => {
  vi.clearAllMocks();
});

/* ═══ Property 17: Hotkey combination formatting ═══════════════════ */
// Feature: gui-feature-completion, Property 17
// **Validates: Requirements 15.4**

describe('Property 17: Hotkey combination formatting', () => {
  const allModifiers = ['Ctrl', 'Shift', 'Alt', 'Meta'] as const;

  it('joins modifiers in canonical order (Ctrl → Shift → Alt → Meta) + key', () => {
    const arbKey = fc.constantFrom('A','B','C','D','E','F','G','M','X','Z','1','2','F1','F2','Space');
    fc.assert(
      fc.property(
        fc.subarray([...allModifiers], { minLength: 0, maxLength: 4 }),
        arbKey,
        (modifiers, key) => {
          const result = formatHotkeyCombination(modifiers, key);
          const parts = result.split('+');
          // Last part is always the key
          expect(parts[parts.length - 1]).toBe(key);
          // Preceding parts are modifiers in canonical order
          const resultMods = parts.slice(0, -1);
          const canonicalOrder = ['Ctrl', 'Shift', 'Alt', 'Meta'];
          const expected = canonicalOrder.filter((m) => (modifiers as readonly string[]).includes(m));
          expect(resultMods).toEqual(expected);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('output is deterministic regardless of modifier input order', () => {
    const arbKey = fc.constantFrom('A','B','C','M','X','Z','1','F1','Space');
    fc.assert(
      fc.property(
        fc.shuffledSubarray([...allModifiers], { minLength: 1, maxLength: 4 }),
        arbKey,
        (modifiers, key) => {
          const result1 = formatHotkeyCombination(modifiers, key);
          const reversed = [...modifiers].reverse();
          const result2 = formatHotkeyCombination(reversed, key);
          expect(result1).toBe(result2);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('no modifiers produces just the key', () => {
    expect(formatHotkeyCombination([], 'M')).toBe('M');
  });

  it('all modifiers produces canonical order', () => {
    expect(formatHotkeyCombination(['Meta', 'Alt', 'Ctrl', 'Shift'], 'M'))
      .toBe('Ctrl+Shift+Alt+Meta+M');
  });

  it('single modifier + key', () => {
    expect(formatHotkeyCombination(['Ctrl'], 'A')).toBe('Ctrl+A');
    expect(formatHotkeyCombination(['Shift'], 'B')).toBe('Shift+B');
  });

  it('default mute hotkey format', () => {
    expect(formatHotkeyCombination(['Ctrl', 'Shift'], 'M')).toBe('Ctrl+Shift+M');
  });
});

/* ═══ Watch All Hotkey Registration ═════════════════════════════════ */

describe('registerWatchAllHotkey', () => {
  it('calls register with the correct shortcut', async () => {
    const callback = vi.fn();
    await registerWatchAllHotkey('CmdOrCtrl+Shift+W', callback);

    expect(register).toHaveBeenCalledTimes(1);
    expect(register).toHaveBeenCalledWith('CmdOrCtrl+Shift+W', expect.any(Function));
  });

  it('registration failure logs warning and does not throw', async () => {
    vi.mocked(register).mockRejectedValueOnce(new Error('Shortcut already taken'));
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

    const callback = vi.fn();
    await expect(registerWatchAllHotkey('CmdOrCtrl+Shift+W', callback)).resolves.toBeUndefined();

    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining('[wavis:hotkey]'),
      expect.stringContaining('failed to register watch-all hotkey'),
      expect.any(String),
    );

    warnSpy.mockRestore();
  });
});

describe('unregisterWatchAllHotkey', () => {
  it('calls unregister with the correct shortcut', async () => {
    await unregisterWatchAllHotkey('CmdOrCtrl+Shift+W');

    expect(unregister).toHaveBeenCalledTimes(1);
    expect(unregister).toHaveBeenCalledWith('CmdOrCtrl+Shift+W');
  });
});
