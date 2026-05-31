import { useEffect, useRef } from 'react';
import type { AuthLogEntry } from './auth';
import { logEntryColor } from './authLog';

interface AuthLogPanelProps {
  title: string;
  logs: AuthLogEntry[];
}

export function AuthLogPanel({ title, logs }: AuthLogPanelProps) {
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [logs]);

  if (logs.length === 0) return null;

  return (
    <div className="border-t border-wavis-text-secondary px-4 sm:px-6 py-4">
      <div className="mb-2 font-bold text-sm">{title}</div>
      <div
        ref={scrollRef}
        className="h-[4.75rem] overflow-y-auto overflow-x-hidden space-y-0.5 text-sm [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
        role="log"
        aria-live="polite"
      >
        {logs.map((entry, i) => (
          <div
            key={i}
            className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]"
            style={{ color: logEntryColor(entry.type) }}
          >
            [{entry.time}] {entry.message}
          </div>
        ))}
      </div>
    </div>
  );
}
