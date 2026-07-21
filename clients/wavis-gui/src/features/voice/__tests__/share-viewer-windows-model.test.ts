/**
 * Tests for share-viewer-windows-model.ts — the window-lifecycle decision
 * points extracted from useShareViewerWindows.ts (1000+ lines of Tauri IPC
 * / MediaStream bookkeeping that can't itself be rendered under vitest's
 * `environment: 'node'` setup; see project conventions — the pattern here
 * is to pull the pure decisions out and test those directly).
 *
 * Covers the three invariants this area has real bug history around:
 *  - a sharer slot never ends up with two live windows (planOpenShareWindow
 *    always tears down whatever's open for that participant first)
 *  - closing/filtering never touches another participant's window
 *    (shouldCloseOnSubRoomChange)
 *  - reopening Watch All reuses the existing window instead of recreating
 *    it (planOpenWatchAllWindow)
 */
import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import {
  planOpenShareWindow,
  planOpenWatchAllWindow,
  decideWatchAllOpenWhenClosed,
  decideWatchAllToggleWhenOpen,
  shouldCloseOnSubRoomChange,
  planAudioOnlySharerBaselineSeed,
} from '../share-viewer-windows-model';
import type { ShareViewerScope } from '../useShareViewerWindows';

/* ═══ planOpenShareWindow — one live window per sharer slot ═══════ */

describe('planOpenShareWindow', () => {
  it('nothing tracked for this participant: no teardown needed', () => {
    const plan = planOpenShareWindow({ natives: new Map(), popouts: new Map() }, 'p1');
    expect(plan).toEqual({ closeExistingNative: false, closeExistingPopout: false });
  });

  it('a native viewer is already open for this participant: must close it first', () => {
    const plan = planOpenShareWindow(
      { natives: new Map([['p1', 'liveKitIdentity']]), popouts: new Map() },
      'p1',
    );
    expect(plan).toEqual({ closeExistingNative: true, closeExistingPopout: false });
  });

  it('a webview pop-out is already open for this participant: must close it first', () => {
    const plan = planOpenShareWindow(
      { natives: new Map(), popouts: new Map([['p1', { scope: 'direct' }]]) },
      'p1',
    );
    expect(plan).toEqual({ closeExistingNative: false, closeExistingPopout: true });
  });

  it('another participant has a window open: this participant is unaffected', () => {
    const registry = {
      natives: new Map([['other', 'x']]),
      popouts: new Map([['another', { scope: 'watch-all' }]]),
    };
    expect(planOpenShareWindow(registry, 'p1')).toEqual({
      closeExistingNative: false,
      closeExistingPopout: false,
    });
    // The other participants' entries are untouched by computing this plan.
    expect(registry.natives.has('other')).toBe(true);
    expect(registry.popouts.has('another')).toBe(true);
  });

  it('property: re-requesting the same participant always reports a teardown, guaranteeing at most one window survives', () => {
    fc.assert(
      fc.property(fc.boolean(), fc.boolean(), (hasNative, hasPopout) => {
        const registry = {
          natives: hasNative ? new Map([['p1', 'x']]) : new Map<string, string>(),
          popouts: hasPopout ? new Map([['p1', { scope: 'direct' as const }]]) : new Map(),
        };
        const plan = planOpenShareWindow(registry, 'p1');
        expect(plan.closeExistingNative).toBe(hasNative);
        expect(plan.closeExistingPopout).toBe(hasPopout);
      }),
      { numRuns: 20 },
    );
  });
});

/* ═══ Watch All: open / toggle / reuse ═════════════════════════════ */

describe('planOpenWatchAllWindow — rejoin reuses the existing window', () => {
  it('already open: reuse (focus) instead of recreating', () => {
    expect(planOpenWatchAllWindow(true)).toBe('focus-existing');
  });

  it('not open: create a new window', () => {
    expect(planOpenWatchAllWindow(false)).toBe('create');
  });
});

describe('decideWatchAllOpenWhenClosed', () => {
  it('remote sharers present: opens', () => {
    expect(decideWatchAllOpenWhenClosed(true)).toBe('open');
  });
  it('no remote sharers: no-op (nothing to show)', () => {
    expect(decideWatchAllOpenWhenClosed(false)).toBe('noop');
  });
});

describe('decideWatchAllToggleWhenOpen', () => {
  it('minimized: restore + focus', () => {
    expect(decideWatchAllToggleWhenOpen(true)).toBe('unminimize');
  });
  it('visible: close', () => {
    expect(decideWatchAllToggleWhenOpen(false)).toBe('close');
  });
});

/* ═══ shouldCloseOnSubRoomChange — stop of A never closes B's window ═══ */

describe('shouldCloseOnSubRoomChange', () => {
  it('a directly-opened pop-out always survives a sub-room switch', () => {
    expect(shouldCloseOnSubRoomChange('direct', 'p1', new Set())).toBe(false);
    expect(shouldCloseOnSubRoomChange('direct', 'p1', new Set(['p1']))).toBe(false);
  });

  it('a watch-all-opened pop-out for a participant still in scope survives', () => {
    expect(shouldCloseOnSubRoomChange('watch-all', 'p1', new Set(['p1', 'p2']))).toBe(false);
  });

  it('a watch-all-opened pop-out for a participant who fell out of scope is closed', () => {
    expect(shouldCloseOnSubRoomChange('watch-all', 'p1', new Set(['p2']))).toBe(true);
  });

  it("closing participant A's window never depends on B's presence in the new scope", () => {
    const newScope = new Set(['p2']);
    // p1 (out of scope) should close regardless of what's in newScope for p2/p3/etc.
    expect(shouldCloseOnSubRoomChange('watch-all', 'p1', newScope)).toBe(true);
    // p2 (in scope) survives independent of p1's fate.
    expect(shouldCloseOnSubRoomChange('watch-all', 'p2', newScope)).toBe(false);
  });

  it('property: the decision for one participant never depends on other ids in the scope set', () => {
    fc.assert(
      fc.property(
        fc.constantFrom<ShareViewerScope>('direct', 'watch-all'),
        fc.string({ minLength: 1, maxLength: 8 }),
        fc.array(fc.string({ minLength: 1, maxLength: 8 }), { maxLength: 5 }),
        fc.boolean(),
        (scope, participantId, otherIds, includeSelf) => {
          const scopeIds = new Set(otherIds.filter((id) => id !== participantId));
          if (includeSelf) scopeIds.add(participantId);

          const decision = shouldCloseOnSubRoomChange(scope, participantId, scopeIds);
          const expected = scope === 'watch-all' && !includeSelf;
          expect(decision).toBe(expected);
        },
      ),
      { numRuns: 30 },
    );
  });
});

/* ═══ Audio-only sharer baseline: rejoin regression (#346) ═════════ */

describe('planAudioOnlySharerBaselineSeed', () => {
  it('not yet joined a sub-room: defers (ShareState not guaranteed settled)', () => {
    expect(planAudioOnlySharerBaselineSeed(false, null, new Set(['p1']))).toBeNull();
  });

  it('regression #346: a share already active when the sub-room settles seeds into the baseline, not treated as new', () => {
    const seed = planAudioOnlySharerBaselineSeed(false, 'room-1', new Set(['p1']));
    expect(seed).toEqual(new Set(['p1']));
  });

  it('already seeded: never reseeds, even if the sub-room id changes', () => {
    expect(planAudioOnlySharerBaselineSeed(true, 'room-1', new Set(['p1']))).toBeNull();
    expect(planAudioOnlySharerBaselineSeed(true, null, new Set())).toBeNull();
  });

  it('no active shares yet when the sub-room settles: seeds an empty baseline', () => {
    expect(planAudioOnlySharerBaselineSeed(false, 'room-1', new Set())).toEqual(new Set());
  });

  it('property: seeding only ever happens once, exactly when unseeded and joined', () => {
    fc.assert(
      fc.property(
        fc.boolean(),
        fc.option(fc.string({ minLength: 1, maxLength: 8 }), { nil: null }),
        fc.array(fc.string({ minLength: 1, maxLength: 8 })),
        (alreadySeeded, joinedSubRoomId, curr) => {
          const currSet = new Set(curr);
          const seed = planAudioOnlySharerBaselineSeed(alreadySeeded, joinedSubRoomId, currSet);
          const shouldSeed = !alreadySeeded && joinedSubRoomId !== null;
          if (shouldSeed) {
            expect(seed).toEqual(currSet);
          } else {
            expect(seed).toBeNull();
          }
        },
      ),
      { numRuns: 30 },
    );
  });
});
