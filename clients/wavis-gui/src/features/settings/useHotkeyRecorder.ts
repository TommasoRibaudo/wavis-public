import { useCallback, useEffect, useRef, useState } from 'react';
import { formatHotkeyCombination } from '@shared/hotkey-bridge';

interface HotkeyKeyEvent {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

export type HotkeyKeydownResult =
  | { kind: 'cancelled' }
  | { kind: 'modifiers'; modifiers: string[] }
  | { kind: 'combo'; combo: string; modifiers: string[] };

/** Pure: derive the next recorder state from a keydown event. */
export function reduceHotkeyKeydown(e: HotkeyKeyEvent): HotkeyKeydownResult {
  if (e.key === 'Escape') return { kind: 'cancelled' };

  const modifiers: string[] = [];
  if (e.ctrlKey) modifiers.push('Ctrl');
  if (e.shiftKey) modifiers.push('Shift');
  if (e.altKey) modifiers.push('Alt');
  if (e.metaKey) modifiers.push('Meta');

  if (['Control', 'Shift', 'Alt', 'Meta'].includes(e.key)) {
    return { kind: 'modifiers', modifiers };
  }

  const mainKey = e.key.length === 1 ? e.key.toUpperCase() : e.key;
  return { kind: 'combo', combo: formatHotkeyCombination(modifiers, mainKey), modifiers };
}

interface UseHotkeyRecorderOptions {
  /** Currently persisted hotkey combo, used as the "old" value to unregister. */
  hotkey: string;
  setHotkeyState: (combo: string) => void;
  persist: (combo: string) => Promise<void>;
  unregister: (hotkey: string) => Promise<void>;
  isRegistered: (hotkey: string) => Promise<boolean>;
  /** Called after the old hotkey is unregistered, e.g. to immediately re-register the new one. */
  onReRegistered?: (combo: string) => void;
}

interface UseHotkeyRecorderResult {
  recording: boolean;
  modifiers: string[];
  error: string | null;
  startRecording: () => void;
}

export function useHotkeyRecorder({
  hotkey,
  setHotkeyState,
  persist,
  unregister,
  isRegistered,
  onReRegistered,
}: UseHotkeyRecorderOptions): UseHotkeyRecorderResult {
  const [recording, setRecording] = useState(false);
  const [modifiers, setModifiers] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  const optionsRef = useRef({
    hotkey,
    setHotkeyState,
    persist,
    unregister,
    isRegistered,
    onReRegistered,
  });
  useEffect(() => {
    optionsRef.current = {
      hotkey,
      setHotkeyState,
      persist,
      unregister,
      isRegistered,
      onReRegistered,
    };
  }, [hotkey, setHotkeyState, persist, unregister, isRegistered, onReRegistered]);

  useEffect(() => {
    if (!recording) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();

      const result = reduceHotkeyKeydown(e);
      if (result.kind === 'cancelled') {
        setRecording(false);
        setModifiers([]);
        setError(null);
        return;
      }
      if (result.kind === 'modifiers') {
        setModifiers(result.modifiers);
        return;
      }

      setModifiers([]);
      setRecording(false);
      setError(null);

      const {
        hotkey: oldHotkey,
        setHotkeyState: setState,
        persist: persistCombo,
        unregister: unregisterOld,
        isRegistered: checkRegistered,
        onReRegistered: reRegistered,
      } = optionsRef.current;
      setState(result.combo);
      void persistCombo(result.combo);

      // Try to re-register if currently registered (active session)
      checkRegistered(oldHotkey)
        .then(async (wasRegistered) => {
          if (wasRegistered) {
            try {
              await unregisterOld(oldHotkey);
            } catch {
              // best effort
            }
            reRegistered?.(result.combo);
          }
        })
        .catch(() => {});
    };

    document.addEventListener('keydown', handleKeyDown, true);
    return () => document.removeEventListener('keydown', handleKeyDown, true);
  }, [recording]);

  const startRecording = useCallback(() => {
    setRecording(true);
    setModifiers([]);
    setError(null);
  }, []);

  return { recording, modifiers, error, startRecording };
}
