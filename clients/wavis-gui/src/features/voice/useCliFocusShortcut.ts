/**
 * Global `/` CLI focus shortcut.
 *
 * Focuses the visible CLI input from anywhere in the room: an idle keypress
 * seeds the input with '/', switches the mobile layout to the log tab, and
 * drives focus both from React's commit phase and a rAF fallback (Tauri's
 * webview can miss either path alone). Both layouts render the CLI input;
 * only the visible one (offsetParent !== null) is focused.
 */

import { useEffect, useRef, type RefObject } from 'react';

interface UseCliFocusShortcutOptions {
  cliInputRef: RefObject<HTMLInputElement | null>;
  /** Current CLI input value — the commit-phase focus keys off its changes. */
  cliInput: string;
  setCliInput: (value: string) => void;
  /** Make the CLI input visible (mobile layout switches to the log tab). */
  onActivate: () => void;
}

export function useCliFocusShortcut({
  cliInputRef,
  cliInput,
  setCliInput,
  onActivate,
}: UseCliFocusShortcutOptions): { focusCliInput: () => void } {
  const pendingCliFocus = useRef(false);
  const activateRef = useRef({ setCliInput, onActivate });

  useEffect(() => {
    activateRef.current = { setCliInput, onActivate };
  }, [setCliInput, onActivate]);

  // Focus whichever CLI input is currently visible in the DOM.
  // Both mobile and desktop layouts may render logPanel simultaneously (same JSX const,
  // same cliInputRef). When the mobile logPanel mounts it captures cliInputRef.current,
  // but in desktop mode that element lives inside an md:hidden (display:none) container
  // and browsers silently ignore focus() on hidden elements. offsetParent === null when
  // an element or any ancestor has display:none, so we use it as a visibility guard and
  // fall back to a data-attribute DOM query to find the actually-visible input.
  const focusCliInput = () => {
    const el = cliInputRef.current;
    if (el && el.offsetParent !== null) {
      el.focus();
      return;
    }
    const inputs = document.querySelectorAll<HTMLInputElement>('[data-cli-input]');
    for (const input of inputs) {
      if (input.offsetParent !== null) {
        input.focus();
        return;
      }
    }
  };
  const focusCliInputRef = useRef(focusCliInput);
  focusCliInputRef.current = focusCliInput;

  // Global `/` shortcut: focus CLI input from anywhere (unless chat is focused)
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key !== '/') return;
      // Don't steal focus when the bug report modal is open.
      if (document.querySelector('[data-bug-report-modal]')) return;
      const activate = () => {
        pendingCliFocus.current = true;
        activateRef.current.setCliInput('/');
        // In mobile/tabbed layout the CLI input lives in the log tab —
        // switch to it first so the input is rendered and visible.
        activateRef.current.onActivate();
        // Fallback: direct focus after React commit + paint.
        // Covers the edge case where cliInput was already '/' (no state
        // change → useEffect doesn't re-fire), and also races the effect
        // to whichever lands first in Tauri's webview.
        requestAnimationFrame(() => focusCliInputRef.current());
      };
      const active = document.activeElement;
      // If typing in the chat input and it's empty, redirect `/` to the CLI input.
      // If the chat input already has text, let the user type normally.
      if (active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement) {
        // Already on a CLI input → let the user type normally.
        // Use the data attribute rather than cliInputRef.current because both
        // layouts render logPanel; the ref may point to the hidden one.
        if (!active.hasAttribute('data-cli-input') && active.value === '') {
          e.preventDefault();
          active.blur();
          activate();
        }
        return;
      }
      e.preventDefault();
      activate();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  // Drive CLI focus from React's commit phase.
  // Two paths ensure focus lands reliably in Tauri's webview:
  // 1. This effect fires when cliInput changes (covers the normal case).
  // 2. The keydown handler also schedules a rAF focus as a fallback —
  //    covers the case where setCliInput('/') is a no-op (value already
  //    '/') so this effect never re-runs.
  useEffect(() => {
    if (pendingCliFocus.current) {
      pendingCliFocus.current = false;
      focusCliInputRef.current();
    }
  }, [cliInput]);

  return { focusCliInput };
}
