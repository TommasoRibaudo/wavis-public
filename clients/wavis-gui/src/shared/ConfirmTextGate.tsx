import { useEffect, useRef, useState } from 'react';

interface ConfirmTextGateProps {
  requiredText: string;
  message?: string;
  busy: boolean;
  busyLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmTextGate({
  requiredText,
  message,
  busy,
  busyLabel = '/confirm',
  onConfirm,
  onCancel,
}: ConfirmTextGateProps) {
  const [value, setValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <div className="border border-wavis-danger p-3 mt-2 bg-wavis-panel">
      {message && <p className="text-wavis-danger text-sm mb-2">{message}</p>}
      <p className="text-wavis-text-secondary text-xs mb-2">Type {requiredText} to confirm</p>
      <div className="flex items-center gap-2">
        <span className="text-wavis-danger shrink-0">&gt;</span>
        <input
          ref={inputRef}
          type="text"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && value === requiredText) onConfirm();
            if (e.key === 'Escape') onCancel();
          }}
          disabled={busy}
          className="flex-1 min-w-0 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text disabled:opacity-40 disabled:cursor-not-allowed"
          aria-label="Confirmation input"
        />
        <button
          onClick={onConfirm}
          disabled={busy || value !== requiredText}
          className="shrink-0 border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {busy ? busyLabel : '/confirm'}
        </button>
      </div>
    </div>
  );
}
