import { CmdButton } from '@shared/CmdButton';

interface ErrorPanelProps {
  error: string;
  onRetry?: () => void;
  className?: string;
}

export function ErrorPanel({
  error,
  onRetry,
  className = '',
}: ErrorPanelProps) {
  return (
    <div className={`p-4 bg-wavis-panel border border-wavis-text-secondary ${className}`}>
      <p className="text-wavis-danger">{error}</p>
      {onRetry && (
        <div className="mt-2">
          <CmdButton label="/retry" onClick={onRetry} />
        </div>
      )}
    </div>
  );
}
