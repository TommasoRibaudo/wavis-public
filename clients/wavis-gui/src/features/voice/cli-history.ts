export type CliHistoryDirection = 'older' | 'newer';

export interface CliHistoryNavigationInput {
  currentInput: string;
  history: string[];
  historyIndex: number;
  draft: string;
  direction: CliHistoryDirection;
}

export interface CliHistoryNavigationResult {
  handled: boolean;
  nextInput: string;
  historyIndex: number;
  draft: string;
}

export function pushCliHistory(history: string[], raw: string): string[] {
  const trimmed = raw.trim();
  if (!trimmed) return history;
  if (history[history.length - 1] === trimmed) return history;
  return [...history, trimmed];
}

export function resetCliHistoryNavigation(): Pick<CliHistoryNavigationResult, 'historyIndex' | 'draft'> {
  return { historyIndex: -1, draft: '' };
}

export function navigateCliHistory(input: CliHistoryNavigationInput): CliHistoryNavigationResult {
  const { currentInput, history, historyIndex, draft, direction } = input;

  if (direction === 'older') {
    if (history.length === 0) {
      return { handled: false, nextInput: currentInput, historyIndex, draft };
    }

    const nextDraft = historyIndex === -1 ? currentInput : draft;
    const nextIndex = Math.min(historyIndex + 1, history.length - 1);
    return {
      handled: true,
      nextInput: history[history.length - 1 - nextIndex],
      historyIndex: nextIndex,
      draft: nextDraft,
    };
  }

  if (historyIndex === -1) {
    return { handled: false, nextInput: currentInput, historyIndex, draft };
  }

  const nextIndex = historyIndex - 1;
  return {
    handled: true,
    nextInput: nextIndex < 0 ? draft : history[history.length - 1 - nextIndex],
    historyIndex: nextIndex,
    draft,
  };
}
