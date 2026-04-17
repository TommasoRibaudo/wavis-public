import { useCallback, useEffect, useRef, useState } from 'react';

interface UseAutoHideOptions {
  delayMs?: number;
  listenToMouseMove?: boolean;
}

export function useAutoHide({ delayMs = 2000, listenToMouseMove = false }: UseAutoHideOptions = {}) {
  const [isVisible, setIsVisible] = useState(true);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const resetTimer = useCallback(() => {
    setIsVisible(true);
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => setIsVisible(false), delayMs);
  }, [delayMs]);

  useEffect(() => {
    if (listenToMouseMove) {
      const onMove = () => resetTimer();
      window.addEventListener('mousemove', onMove);
      timerRef.current = setTimeout(() => setIsVisible(false), delayMs);
      return () => {
        window.removeEventListener('mousemove', onMove);
        if (timerRef.current) clearTimeout(timerRef.current);
      };
    }

    resetTimer();
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [delayMs, listenToMouseMove, resetTimer]);

  return { isVisible, resetTimer };
}
