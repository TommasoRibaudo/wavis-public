import { describe, expect, it } from 'vitest';
import {
  navigateCliHistory,
  pushCliHistory,
  resetCliHistoryNavigation,
} from '../cli-history';

describe('pushCliHistory', () => {
  it('appends trimmed commands', () => {
    expect(pushCliHistory([], '  /mute  ')).toEqual(['/mute']);
  });

  it('ignores blank commands', () => {
    const history = ['/mute'];
    expect(pushCliHistory(history, '   ')).toBe(history);
  });

  it('deduplicates consecutive commands only', () => {
    const history = ['/mute', '/help'];
    expect(pushCliHistory(history, '/help')).toBe(history);
    expect(pushCliHistory(history, '/mute')).toEqual(['/mute', '/help', '/mute']);
  });
});

describe('navigateCliHistory', () => {
  it('does nothing when navigating older with no history', () => {
    expect(
      navigateCliHistory({
        currentInput: '',
        history: [],
        historyIndex: -1,
        draft: '',
        direction: 'older',
      }),
    ).toEqual({
      handled: false,
      nextInput: '',
      historyIndex: -1,
      draft: '',
    });
  });

  it('walks backward through older commands and captures the draft once', () => {
    const first = navigateCliHistory({
      currentInput: '/mu',
      history: ['/help', '/mute', '/share'],
      historyIndex: -1,
      draft: '',
      direction: 'older',
    });

    expect(first).toEqual({
      handled: true,
      nextInput: '/share',
      historyIndex: 0,
      draft: '/mu',
    });

    const second = navigateCliHistory({
      currentInput: first.nextInput,
      history: ['/help', '/mute', '/share'],
      historyIndex: first.historyIndex,
      draft: first.draft,
      direction: 'older',
    });

    expect(second).toEqual({
      handled: true,
      nextInput: '/mute',
      historyIndex: 1,
      draft: '/mu',
    });
  });

  it('walks forward toward the draft and restores it at the end', () => {
    const first = navigateCliHistory({
      currentInput: '/mute',
      history: ['/help', '/mute', '/share'],
      historyIndex: 1,
      draft: '/mu',
      direction: 'newer',
    });

    expect(first).toEqual({
      handled: true,
      nextInput: '/share',
      historyIndex: 0,
      draft: '/mu',
    });

    const second = navigateCliHistory({
      currentInput: first.nextInput,
      history: ['/help', '/mute', '/share'],
      historyIndex: first.historyIndex,
      draft: first.draft,
      direction: 'newer',
    });

    expect(second).toEqual({
      handled: true,
      nextInput: '/mu',
      historyIndex: -1,
      draft: '/mu',
    });
  });

  it('does nothing when navigating newer outside history browsing', () => {
    expect(
      navigateCliHistory({
        currentInput: '/mu',
        history: ['/help'],
        historyIndex: -1,
        draft: '',
        direction: 'newer',
      }),
    ).toEqual({
      handled: false,
      nextInput: '/mu',
      historyIndex: -1,
      draft: '',
    });
  });
});

describe('resetCliHistoryNavigation', () => {
  it('returns the neutral browsing state', () => {
    expect(resetCliHistoryNavigation()).toEqual({ historyIndex: -1, draft: '' });
  });
});
