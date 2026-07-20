import { useEffect, useLayoutEffect, useState, type RefObject } from 'react';
import { CmdButton } from '@shared/CmdButton';

export interface TutorialStep {
  title: string;
  body: string;
  /** Which element on the channels list this step points at. */
  highlight: 'create' | 'join' | 'list' | null;
}

export const TUTORIAL_STEPS: TutorialStep[] = [
  {
    title: 'create a room',
    body: 'Click /create in the bar below, type a name for your room, and hit Enter — your room is ready instantly.',
    highlight: 'create',
  },
  {
    title: 'join a room',
    body: 'Got an invite code from a teammate? Click /join, paste the code, and hit Enter to join their room.',
    highlight: 'join',
  },
  {
    title: 'enter a room',
    body: 'Click any room in the list to enter it. Once inside you can /mute or /deafen yourself, turn on your camera, share your screen, and switch rooms without leaving the call.',
    highlight: 'list',
  },
];

interface TutorialOverlayProps {
  step: number;
  targetRef: RefObject<HTMLElement | null> | null;
  onNext: () => void;
  onBack: () => void;
  onDismiss: () => void;
}

const DIALOG_WIDTH = 480;
const VIEWPORT_MARGIN = 16;
const TARGET_GAP = 14;
const ARROW_MARGIN = 24;

interface Anchor {
  top: number;
  bottom: number;
  centerX: number;
}

export interface TutorialPlacement {
  placeBelow: boolean;
  dialogLeft: number | null;
  arrowLeft: number | null;
}

/**
 * Positions the dialog directly against its anchor target, clamped to stay
 * fully inside the viewport, with the arrow offset recomputed to still point
 * at the target's true center even when the dialog itself got clamped away
 * from center (e.g. a target near the window edge).
 */
export function computeTutorialPlacement(
  anchor: Anchor | null,
  viewport: { width: number; height: number },
): TutorialPlacement {
  if (!anchor) return { placeBelow: false, dialogLeft: null, arrowLeft: null };

  const placeBelow = anchor.top < viewport.height / 2;
  const dialogLeft = Math.min(
    Math.max(anchor.centerX - DIALOG_WIDTH / 2, VIEWPORT_MARGIN),
    viewport.width - DIALOG_WIDTH - VIEWPORT_MARGIN,
  );
  const arrowLeft = Math.min(
    Math.max(anchor.centerX - dialogLeft, ARROW_MARGIN),
    DIALOG_WIDTH - ARROW_MARGIN,
  );

  return { placeBelow, dialogLeft, arrowLeft };
}

export function TutorialOverlay({
  step,
  targetRef,
  onNext,
  onBack,
  onDismiss,
}: TutorialOverlayProps) {
  const isLast = step === TUTORIAL_STEPS.length - 1;
  const current = TUTORIAL_STEPS[step];
  const [anchor, setAnchor] = useState<Anchor | null>(null);

  // Re-measures the target on every step change (a different element) and on
  // resize (app window is user-resizable, min 480x400 — positions shift).
  useLayoutEffect(() => {
    function measure() {
      const el = targetRef?.current;
      if (!el) {
        setAnchor(null);
        return;
      }
      const rect = el.getBoundingClientRect();
      setAnchor({ top: rect.top, bottom: rect.bottom, centerX: rect.left + rect.width / 2 });
    }
    measure();
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, [targetRef, step]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onDismiss();
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [onDismiss]);

  // Targets in the top half of the window get the dialog placed below them
  // (arrow pointing up); targets in the bottom half get it placed above
  // (arrow pointing down) — same idea as the old bottom-command-bar-only
  // anchor, generalized to any target position.
  const { placeBelow, dialogLeft, arrowLeft } = computeTutorialPlacement(anchor, {
    width: window.innerWidth,
    height: window.innerHeight,
  });

  const dialogStyle =
    anchor && dialogLeft !== null
      ? placeBelow
        ? { position: 'fixed' as const, top: anchor.bottom + TARGET_GAP, left: dialogLeft }
        : {
            position: 'fixed' as const,
            top: anchor.top - TARGET_GAP,
            left: dialogLeft,
            transform: 'translateY(-100%)',
          }
      : { position: 'fixed' as const, left: '50%', bottom: '5rem', transform: 'translateX(-50%)' };

  return (
    <div
      role="dialog"
      aria-label="Wavis tutorial"
      style={dialogStyle}
      className="z-50 w-[480px] max-w-full bg-wavis-bg border border-wavis-accent font-mono text-wavis-text p-6 flex flex-col gap-4 shadow-lg"
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
          {step > 0 && <CmdButton label="/back" onClick={onBack} />}
          {!isLast && <CmdButton label="/next" active onClick={onNext} />}
          {isLast && <CmdButton label="/done" active onClick={onDismiss} />}
        </div>
      </div>

      {arrowLeft !== null &&
        (placeBelow ? (
          <div
            aria-hidden="true"
            style={{ left: arrowLeft }}
            className="absolute bottom-full -mb-[7px] -translate-x-1/2 h-3 w-3 rotate-45 border-t border-l border-wavis-accent bg-wavis-bg"
          />
        ) : (
          <div
            aria-hidden="true"
            style={{ left: arrowLeft }}
            className="absolute top-full -mt-[7px] -translate-x-1/2 h-3 w-3 rotate-45 border-b border-r border-wavis-accent bg-wavis-bg"
          />
        ))}
    </div>
  );
}
