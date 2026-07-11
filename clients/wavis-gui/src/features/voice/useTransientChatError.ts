/**
 * Transient chat error toast state: surfaces the latest signaling chat error
 * while the chat panel is visible, auto-dismissing after 5s. On mobile the
 * error is silently dropped unless the chat tab is active.
 */

import { useEffect, useRef, useState } from 'react';

const CHAT_ERROR_DISMISS_MS = 5_000;

export function useTransientChatError(
  lastChatError: string | null | undefined,
  mobileTab: string,
): string | null {
  const [chatError, setChatError] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const prevErrorRef = useRef<string | null>(null);

  useEffect(() => {
    const err = lastChatError ?? null;
    if (err === prevErrorRef.current) return;
    prevErrorRef.current = err;
    if (!err) return;

    // On mobile, drop errors when chat tab is not active
    // On desktop (md+), chat panel is always visible — use matchMedia to detect
    const isDesktop = window.matchMedia('(min-width: 768px)').matches;
    if (!isDesktop && mobileTab !== 'chat') {
      return; // silently drop
    }

    // Clear any existing timer
    if (timerRef.current) clearTimeout(timerRef.current);

    setChatError(err);
    timerRef.current = setTimeout(() => {
      setChatError(null);
      timerRef.current = null;
    }, CHAT_ERROR_DISMISS_MS);
  }, [lastChatError, mobileTab]);

  // Clean up the dismiss timer on unmount
  useEffect(() => {
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, []);

  return chatError;
}
