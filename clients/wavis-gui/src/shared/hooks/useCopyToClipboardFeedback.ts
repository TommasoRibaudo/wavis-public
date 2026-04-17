import { useCallback, useEffect, useRef, useState } from 'react';

interface Options {
  feedbackMs?: number;
  writeText?: (text: string) => Promise<void>;
}

export function useCopyToClipboardFeedback(options: Options = {}) {
  const { feedbackMs = 2000, writeText } = options;
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, []);

  const copy = useCallback(async (text: string) => {
    try {
      const writer = writeText ?? ((t: string) => navigator.clipboard.writeText(t));
      await writer(text);
      if (timerRef.current) clearTimeout(timerRef.current);
      setCopied(true);
      timerRef.current = setTimeout(() => setCopied(false), feedbackMs);
    } catch {
      // Silent — caller is responsible for fallback if needed.
    }
  }, [feedbackMs, writeText]);

  return [copy, copied] as const;
}
