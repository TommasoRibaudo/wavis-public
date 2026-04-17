import type { KeyboardEventHandler, Ref } from 'react';

interface AuthFieldRowProps {
  label: string;
  type?: 'text' | 'password';
  value: string;
  onChange: (value: string) => void;
  onKeyDown?: KeyboardEventHandler<HTMLInputElement>;
  error?: string | null;
  disabled?: boolean;
  placeholder?: string;
  maxLength?: number;
  inputRef?: Ref<HTMLInputElement>;
  ariaLabel?: string;
  autoFocus?: boolean;
  className?: string;
}

export function AuthFieldRow({
  label,
  type = 'text',
  value,
  onChange,
  onKeyDown,
  error,
  disabled,
  placeholder,
  maxLength,
  inputRef,
  ariaLabel,
  autoFocus,
  className = 'mb-4',
}: AuthFieldRowProps) {
  return (
    <div className={className}>
      <div className="mb-2 text-sm">{label}</div>
      <div className="flex items-center gap-2">
        <span className="text-wavis-accent shrink-0">&gt;</span>
        <input
          ref={inputRef}
          type={type}
          placeholder={placeholder}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={onKeyDown}
          disabled={disabled}
          maxLength={maxLength}
          className={`flex-1 min-w-0 bg-transparent border-b outline-none px-2 py-1 font-mono text-wavis-text ${
            error ? 'border-wavis-danger' : 'border-wavis-text-secondary'
          } disabled:opacity-40 disabled:cursor-not-allowed`}
          aria-label={ariaLabel ?? label.replace(/:$/, '')}
          autoFocus={autoFocus}
          autoComplete={type === 'password' ? 'off' : undefined}
        />
      </div>
      {error && (
        <p className="text-wavis-danger text-sm mt-1 pl-6">{error}</p>
      )}
    </div>
  );
}
