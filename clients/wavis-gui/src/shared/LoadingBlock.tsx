interface LoadingBlockProps {
  message?: string;
  className?: string;
}

export function LoadingBlock({
  message = 'loading...',
  className = 'p-4 text-center',
}: LoadingBlockProps) {
  return (
    <div className={`text-wavis-text-secondary ${className}`}>
      {message}
    </div>
  );
}
