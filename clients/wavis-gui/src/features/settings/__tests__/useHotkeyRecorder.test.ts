import { describe, it, expect } from 'vitest';
import { reduceHotkeyKeydown } from '../useHotkeyRecorder';

function keyEvent(
  key: string,
  mods: Partial<{ ctrlKey: boolean; shiftKey: boolean; altKey: boolean; metaKey: boolean }> = {},
) {
  return {
    key,
    ctrlKey: mods.ctrlKey ?? false,
    shiftKey: mods.shiftKey ?? false,
    altKey: mods.altKey ?? false,
    metaKey: mods.metaKey ?? false,
  };
}

describe('reduceHotkeyKeydown', () => {
  it('Escape cancels regardless of modifiers', () => {
    expect(reduceHotkeyKeydown(keyEvent('Escape', { ctrlKey: true }))).toEqual({
      kind: 'cancelled',
    });
  });

  it('a bare modifier key reports collected modifiers without finalizing', () => {
    expect(reduceHotkeyKeydown(keyEvent('Control', { ctrlKey: true }))).toEqual({
      kind: 'modifiers',
      modifiers: ['Ctrl'],
    });
    expect(reduceHotkeyKeydown(keyEvent('Shift', { shiftKey: true }))).toEqual({
      kind: 'modifiers',
      modifiers: ['Shift'],
    });
  });

  it('a main key finalizes the combo in canonical modifier order', () => {
    expect(reduceHotkeyKeydown(keyEvent('m', { ctrlKey: true, shiftKey: true }))).toEqual({
      kind: 'combo',
      combo: 'Ctrl+Shift+M',
      modifiers: ['Ctrl', 'Shift'],
    });
  });

  it('single-character keys are uppercased; named keys pass through unchanged', () => {
    expect(reduceHotkeyKeydown(keyEvent('a'))).toEqual({
      kind: 'combo',
      combo: 'A',
      modifiers: [],
    });
    expect(reduceHotkeyKeydown(keyEvent('F5'))).toEqual({
      kind: 'combo',
      combo: 'F5',
      modifiers: [],
    });
  });

  it('modifier order in output is Ctrl, Shift, Alt, Meta regardless of press order', () => {
    const result = reduceHotkeyKeydown(
      keyEvent('x', { metaKey: true, altKey: true, ctrlKey: true, shiftKey: true }),
    );
    expect(result).toEqual({
      kind: 'combo',
      combo: 'Ctrl+Shift+Alt+Meta+X',
      modifiers: ['Ctrl', 'Shift', 'Alt', 'Meta'],
    });
  });
});
