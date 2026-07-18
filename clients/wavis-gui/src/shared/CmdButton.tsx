import { forwardRef } from 'react';

interface CmdButtonProps {
  label: string;
  onClick: () => void;
  active?: boolean;
  danger?: boolean;
  disabled?: boolean;
  highlight?: boolean;
  className?: string;
}

export const CmdButton = forwardRef<HTMLButtonElement, CmdButtonProps>(function CmdButton(
  { label, onClick, active, danger, disabled, highlight, className },
  ref,
) {
  return (
    <button
      ref={ref}
      onClick={onClick}
      disabled={disabled}
      className={`disabled:opacity-40 disabled:cursor-not-allowed border text-center transition-colors ${className ?? 'text-xs px-1 py-0.5'} ${
        danger
          ? 'border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg'
          : active
            ? 'border-wavis-accent text-wavis-accent bg-wavis-accent/8 hover:bg-wavis-accent hover:text-wavis-bg'
            : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'
      } ${highlight ? 'ring-2 ring-wavis-accent ring-offset-2 ring-offset-wavis-bg animate-pulse' : ''}`}
    >
      {label}
    </button>
  );
});
