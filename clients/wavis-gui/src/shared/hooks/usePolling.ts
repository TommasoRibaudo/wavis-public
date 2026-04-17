import { useEffect, useRef } from 'react';

export function usePolling(
  fn: () => void,
  intervalMs: number,
  options?: { enabled?: boolean }
): void {
  const fnRef = useRef(fn);
  useEffect(() => {
    fnRef.current = fn;
  }, [fn]);

  useEffect(() => {
    if (options?.enabled === false) return;
    const id = setInterval(() => fnRef.current(), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs, options?.enabled]);
}
