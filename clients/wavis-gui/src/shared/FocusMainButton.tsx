import { useHotkeys } from '@shared/useHotkeys';

interface FocusMainButtonProps {
  /** Called when the user clicks the button. The handler is responsible
   *  for calling setFocus() on the appropriate window — `getCurrentWindow()`
   *  from a main-window context, `WebviewWindow.getByLabel('main')` from
   *  a popup window. */
  onClick: () => void;
}

/**
 * Small icon button that requests focus on the main Wavis window.
 * Used by the screen-share/watch-all popup hover bars and bottom bars.
 *
 * Stops mousedown/click propagation to avoid interfering with the
 * draggable region / hover-bar parent that hosts it.
 */
export default function FocusMainButton({ onClick }: FocusMainButtonProps) {
  const { focusMain } = useHotkeys();
  return (
    <button
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className="px-1.5 flex items-center justify-center text-wavis-text-secondary hover:opacity-70 transition-opacity"
      aria-label="Focus main window"
      title={`focus main window (${focusMain})`}
    >
      ⌂
    </button>
  );
}
