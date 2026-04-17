import { useCallback, useEffect, useRef, useState } from 'react';

interface UseShareReconnectOptions {
  onTrigger?: () => void;
}

export function useShareReconnect({ onTrigger }: UseShareReconnectOptions = {}) {
  const [retryCount, setRetryCount] = useState(0);
  const onTriggerRef = useRef(onTrigger);

  useEffect(() => {
    onTriggerRef.current = onTrigger;
  }, [onTrigger]);

  const triggerReconnect = useCallback(() => {
    onTriggerRef.current?.();
    setRetryCount((c) => c + 1);
  }, []);

  return { retryCount, triggerReconnect };
}
