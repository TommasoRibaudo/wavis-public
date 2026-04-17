interface EmptyStateProps {
  message: string;
  className?: string;
}

export function EmptyState({ message, className = '' }: EmptyStateProps) {
  return (
    <div className={`bg-wavis-panel border border-wavis-text-secondary ${className}`}>
      <p className="text-wavis-text-secondary text-sm">{message}</p>
    </div>
  );
}
