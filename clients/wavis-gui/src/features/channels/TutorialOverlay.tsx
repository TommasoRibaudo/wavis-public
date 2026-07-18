import { useEffect, useState } from 'react';
import { CmdButton } from '@shared/CmdButton';

interface TutorialStep {
  title: string;
  body: string;
}

export const TUTORIAL_STEPS: TutorialStep[] = [
  {
    title: 'create a room',
    body: 'Click /create in the bar below, type a name for your room, and hit Enter — your room is ready instantly.',
  },
  {
    title: 'join a room',
    body: 'Got an invite code from a teammate? Click /join, paste the code, and hit Enter to join their room.',
  },
  {
    title: 'enter a room',
    body: 'Click any room in the list to enter it. Once inside you can /mute or /deafen yourself, turn on your camera, share your screen, and switch rooms without leaving the call.',
  },
];

interface TutorialOverlayProps {
  onDismiss: () => void;
}

export function TutorialOverlay({ onDismiss }: TutorialOverlayProps) {
  const [step, setStep] = useState(0);
  const isLast = step === TUTORIAL_STEPS.length - 1;
  const current = TUTORIAL_STEPS[step];

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onDismiss();
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [onDismiss]);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-wavis-overlay-base/60">
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Wavis tutorial"
        className="w-[480px] max-w-[95vw] bg-wavis-bg border border-wavis-text-secondary font-mono text-wavis-text p-6 flex flex-col gap-4 shadow-lg"
      >
        <div className="flex items-center justify-between">
          <span className="text-sm text-wavis-accent">▲ welcome to wavis</span>
          <button
            onClick={onDismiss}
            aria-label="Skip tutorial"
            className="text-wavis-text-secondary hover:text-wavis-text text-xs"
          >
            /skip
          </button>
        </div>

        <div>
          <p className="text-sm text-wavis-accent mb-2">
            {step + 1}/{TUTORIAL_STEPS.length} — {current.title}
          </p>
          <p className="text-sm text-wavis-text leading-relaxed">{current.body}</p>
        </div>

        <div className="flex items-center justify-between mt-2">
          <div className="flex gap-1" aria-hidden="true">
            {TUTORIAL_STEPS.map((s, i) => (
              <span
                key={s.title}
                className={`h-1.5 w-1.5 rounded-full ${
                  i === step ? 'bg-wavis-accent' : 'bg-wavis-text-secondary'
                }`}
              />
            ))}
          </div>
          <div className="flex gap-2">
            {step > 0 && <CmdButton label="/back" onClick={() => setStep((s) => s - 1)} />}
            {!isLast && <CmdButton label="/next" active onClick={() => setStep((s) => s + 1)} />}
            {isLast && <CmdButton label="/done" active onClick={onDismiss} />}
          </div>
        </div>
      </div>
    </div>
  );
}
