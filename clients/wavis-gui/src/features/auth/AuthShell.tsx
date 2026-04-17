import type { ReactNode } from 'react';

interface AuthShellProps {
  subtitle: ReactNode;
  children: ReactNode;
}

export function AuthShell({ subtitle, children }: AuthShellProps) {
  return (
    <div className="h-screen flex items-center justify-center p-4 bg-wavis-bg font-mono text-wavis-text">
      <div className="w-full max-w-[640px] border border-wavis-text-secondary bg-wavis-panel">
        <div className="px-4 sm:px-6 pt-6 pb-4 border-b border-wavis-text-secondary">
          <div className="font-bold">WAVIS</div>
          <div className="text-wavis-text-secondary overflow-hidden">────────────</div>
          {subtitle}
        </div>
        {children}
      </div>
    </div>
  );
}
