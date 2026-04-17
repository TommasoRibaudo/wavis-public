import { getCurrentWindow } from '@tauri-apps/api/window';
import { Minus, Square, X } from 'lucide-react';

/* ─── Helpers ───────────────────────────────────────────────────── */
const appWindow = getCurrentWindow();

function WindowButton({ onClick, danger, label, children }: {
  onClick: () => void;
  danger?: boolean;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <button
      aria-label={label}
      onClick={onClick}
      className={`inline-flex items-center justify-center w-11 h-8 text-wavis-text-secondary transition-colors ${
        danger
          ? 'hover:bg-wavis-danger hover:text-wavis-text-contrast'
          : 'hover:bg-wavis-panel hover:text-wavis-text'
      }`}
    >
      {children}
    </button>
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */
export default function TitleBar() {
  return (
    <div
      data-tauri-drag-region
      className="flex items-center justify-between h-8 bg-wavis-bg border-b border-wavis-panel select-none shrink-0"
    >
      {/* App title */}
      <div className="flex items-center gap-2 pl-3 font-mono text-xs text-wavis-text-secondary">
        <span className="text-wavis-accent">▸</span>
        <span>wavis</span>
      </div>

      {/* Window controls */}
      <div className="flex">
        <WindowButton label="Minimize" onClick={() => appWindow.minimize()}>
          <Minus size={14} />
        </WindowButton>
        <WindowButton label="Maximize" onClick={() => appWindow.toggleMaximize()}>
          <Square size={11} />
        </WindowButton>
        <WindowButton label="Close" onClick={() => appWindow.close()} danger>
          <X size={14} />
        </WindowButton>
      </div>
    </div>
  );
}
