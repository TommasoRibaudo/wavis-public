/**
 * Auto-scroll anchor: returns a ref to place on a sentinel element at the
 * bottom of a list; whenever `dep` changes, the sentinel is scrolled into
 * view (smooth). Used by the room event log and chat panels.
 */

import { useEffect, useRef, type RefObject } from 'react';

export function useAutoScrollAnchor<T extends HTMLElement>(dep: unknown): RefObject<T | null> {
  const anchorRef = useRef<T>(null);

  useEffect(() => {
    anchorRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [dep]);

  return anchorRef;
}
