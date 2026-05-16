interface CmdButtonProps {
  label: string;
  onClick: () => void;
  active?: boolean;
  danger?: boolean;
  disabled?: boolean;
  className?: string;
}

export function CmdButton({
  label,
  onClick,
  active,
  danger,
  disabled,
  className,
}: CmdButtonProps) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`text-xs disabled:opacity-40 disabled:cursor-not-allowed border px-1 text-center transition-colors ${className ?? 'py-0.5'} ${
        danger
          ? 'border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg'
          : active
            ? 'border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg'
            : 'border-wavis-text-secondary text-wavis-text hover:bg-wavis-text-secondary hover:text-wavis-text-contrast'
      }`}
    >
      {label}
    </button>
  );
}
