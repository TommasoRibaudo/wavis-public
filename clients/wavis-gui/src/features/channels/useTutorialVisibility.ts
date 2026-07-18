import { useCallback, useEffect, useState } from 'react';
import { getHasSeenTutorial, setHasSeenTutorial } from '@features/settings/settings-store';
import { TUTORIAL_STEPS } from './TutorialOverlay';

const LAST_STEP = TUTORIAL_STEPS.length - 1;

/** Shows the first-time tutorial once per account, and lets it be reopened on demand. */
export function useTutorialVisibility() {
  const [showTutorial, setShowTutorial] = useState(false);
  const [step, setStep] = useState(0);

  useEffect(() => {
    let cancelled = false;
    void getHasSeenTutorial().then((seen) => {
      if (!cancelled && !seen) setShowTutorial(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const openTutorial = useCallback(() => {
    setStep(0);
    setShowTutorial(true);
  }, []);

  const dismissTutorial = useCallback(() => {
    setShowTutorial(false);
    void setHasSeenTutorial(true);
  }, []);

  const nextStep = useCallback(() => setStep((s) => Math.min(s + 1, LAST_STEP)), []);
  const backStep = useCallback(() => setStep((s) => Math.max(s - 1, 0)), []);

  return { showTutorial, step, openTutorial, dismissTutorial, nextStep, backStep };
}
