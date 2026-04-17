/**
 * Property tests for tray bridge pure helpers.
 *
 * Property 12: Tray menu items disabled when no voice session
 * Property 13: Mute label reflects mute state
 *
 * Validates: Requirements 1.3, 1.5
 */

import { describe, it, expect, vi } from 'vitest';
import fc from 'fast-check';

/* ─── Mock @tauri-apps/api/event ────────────────────────────────── */

vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn().mockResolvedValue(undefined),
  listen: vi.fn().mockResolvedValue(() => {}),
}));

/* ─── Import after mock ─────────────────────────────────────────── */

import { computeTrayMenuState, muteMenuLabel } from '../tray-bridge';
import type { TrayStateUpdate } from '../tray-bridge';

/* ─── Arbitraries ───────────────────────────────────────────────── */

const arbTrayStateUpdate: fc.Arbitrary<TrayStateUpdate> = fc.record({
  inVoiceSession: fc.boolean(),
  isMuted: fc.boolean(),
});

/* ═══ Property 12: Tray menu items disabled when no voice session ══ */
// Feature: gui-feature-completion, Property 12
// **Validates: Requirements 1.3**

describe('Property 12: Tray menu items disabled when no voice session', () => {
  it('when inVoiceSession is false, mute and leave are disabled', () => {
    fc.assert(
      fc.property(
        arbTrayStateUpdate.filter((u) => !u.inVoiceSession),
        (update) => {
          const state = computeTrayMenuState(update);
          expect(state.muteEnabled).toBe(false);
          expect(state.leaveEnabled).toBe(false);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('when inVoiceSession is true, mute and leave are enabled', () => {
    fc.assert(
      fc.property(
        arbTrayStateUpdate.filter((u) => u.inVoiceSession),
        (update) => {
          const state = computeTrayMenuState(update);
          expect(state.muteEnabled).toBe(true);
          expect(state.leaveEnabled).toBe(true);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('muteEnabled and leaveEnabled always equal inVoiceSession', () => {
    fc.assert(
      fc.property(arbTrayStateUpdate, (update) => {
        const state = computeTrayMenuState(update);
        expect(state.muteEnabled).toBe(update.inVoiceSession);
        expect(state.leaveEnabled).toBe(update.inVoiceSession);
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 13: Mute label reflects mute state ══════════════════ */
// Feature: gui-feature-completion, Property 13
// **Validates: Requirements 1.5**

describe('Property 13: Mute label reflects mute state', () => {
  it('when isMuted is true, label is "Unmute"', () => {
    fc.assert(
      fc.property(fc.constant(true), (isMuted) => {
        expect(muteMenuLabel(isMuted)).toBe('Unmute');
      }),
      { numRuns: 1 },
    );
  });

  it('when isMuted is false, label is "Mute"', () => {
    fc.assert(
      fc.property(fc.constant(false), (isMuted) => {
        expect(muteMenuLabel(isMuted)).toBe('Mute');
      }),
      { numRuns: 1 },
    );
  });

  it('for any boolean isMuted, label is "Unmute" iff muted, "Mute" iff not', () => {
    fc.assert(
      fc.property(fc.boolean(), (isMuted) => {
        const label = muteMenuLabel(isMuted);
        if (isMuted) {
          expect(label).toBe('Unmute');
        } else {
          expect(label).toBe('Mute');
        }
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 7: Close behavior follows minimize-to-tray setting ══ */
// Feature: gui-feature-completion, Property 7
// **Validates: Requirements 18.1, 18.2**

import { shouldHideOnClose } from '../tray-bridge';

describe('Property 7: Close behavior follows minimize-to-tray setting', () => {
  it('window is hidden on close iff minimizeToTray is true', () => {
    fc.assert(
      fc.property(fc.boolean(), (minimizeToTray) => {
        const shouldHide = shouldHideOnClose(minimizeToTray);
        expect(shouldHide).toBe(minimizeToTray);
      }),
      { numRuns: 100 },
    );
  });

  it('when minimizeToTray is true, close hides the window', () => {
    expect(shouldHideOnClose(true)).toBe(true);
  });

  it('when minimizeToTray is false, close proceeds normally', () => {
    expect(shouldHideOnClose(false)).toBe(false);
  });
});
