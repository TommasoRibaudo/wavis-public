import type { ReactNode } from 'react';

interface AuthShellProps {
  subtitle: ReactNode;
  children: ReactNode;
}

export function AuthShell({ subtitle, children }: AuthShellProps) {
  return (
    <div className="h-full min-h-0 overflow-y-auto overflow-x-hidden bg-wavis-bg font-mono text-wavis-text">
      <div className="box-border min-h-full w-full grid place-items-center p-1 min-[360px]:p-2 sm:p-4">
        <div className="w-full max-w-[640px] border border-wavis-text-secondary bg-wavis-panel">
          <div className="px-3 min-[360px]:px-4 sm:px-6 pt-3 min-[520px]:pt-4 sm:pt-6 pb-3 sm:pb-4 border-b border-wavis-text-secondary">
            <div className="font-bold">WAVIS</div>
            <div className="text-wavis-text-secondary overflow-hidden">────────────</div>
            {subtitle}
          </div>
          {children}
        </div>
      </div>
    </div>
  );
}
