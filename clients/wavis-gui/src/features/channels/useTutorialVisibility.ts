import { useCallback, useEffect, useState } from 'react';
import { getHasSeenTutorial, setHasSeenTutorial } from '@features/settings/settings-store';

/** Shows the first-time tutorial once per account, and lets it be reopened on demand. */
export function useTutorialVisibility() {
  const [showTutorial, setShowTutorial] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void getHasSeenTutorial().then((seen) => {
      if (!cancelled && !seen) setShowTutorial(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const openTutorial = useCallback(() => setShowTutorial(true), []);

  const dismissTutorial = useCallback(() => {
    setShowTutorial(false);
    void setHasSeenTutorial(true);
  }, []);

  return { showTutorial, openTutorial, dismissTutorial };
}
