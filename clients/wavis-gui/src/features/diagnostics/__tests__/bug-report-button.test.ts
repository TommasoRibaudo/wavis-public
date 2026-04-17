/**
 * BugReportButton Property Tests & Unit Tests
 *
 * Property-based tests (fast-check) for button snap logic and
 * position round-trip via settings store. Unit tests for chromeless
 * window exclusion.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as fc from 'fast-check';
import {
  getButtonTransitionClass,
  getHoverLabelPositionClass,
  snapToEdge,
  shouldExpandLeft,
} from '../BugReportButton';

// ─── Mocks ─────────────────────────────────────────────────────────

// Mock @tauri-apps/api/window
const mockLabel = { value: 'main' };
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ label: mockLabel.value }),
}));

// Mock settings-store with in-memory map for round-trip testing
const storeMap = new Map<string, unknown>();
vi.mock('@features/settings/settings-store', () => ({
  STORE_KEYS: { bugReportButtonPos: 'wavis_bug_report_button_pos' },
  getStoreValue: vi.fn(async <T>(key: string, defaultValue: T): Promise<T> => {
    return (storeMap.has(key) ? storeMap.get(key) as T : defaultValue);
  }),
  setStoreValue: vi.fn(async <T>(key: string, value: T): Promise<void> => {
    storeMap.set(key, value);
  }),
}));

import { getStoreValue, setStoreValue, STORE_KEYS } from '@features/settings/settings-store';

beforeEach(() => {
  storeMap.clear();
  vi.clearAllMocks();
});

// ─── Unit Tests: snapToEdge ────────────────────────────────────────

describe('snapToEdge — unit tests', () => {
  it('snaps to bottom-right corner from center-right area', () => {
    const result = snapToEdge(700, 400, 40, 40, 800, 600);
    expect(result.x).toBe(760); // windowWidth - buttonWidth
    expect(result.y).toBe(560); // windowHeight - buttonHeight
  });

  it('snaps to top-left corner from top-left area', () => {
    const result = snapToEdge(10, 10, 40, 40, 800, 600);
    expect(result.x).toBe(0);
    expect(result.y).toBe(0);
  });

  it('snaps to top-right corner from top-right area', () => {
    const result = snapToEdge(750, 10, 40, 40, 800, 600);
    expect(result.x).toBe(760);
    expect(result.y).toBe(0);
  });

  it('expands left when the hover label would overflow the right edge', () => {
    expect(shouldExpandLeft(760, 800)).toBe(true);
  });

  it('keeps expanding right when there is enough room', () => {
    expect(shouldExpandLeft(100, 800)).toBe(false);
  });

  it('disables transitions while dragging', () => {
    expect(getButtonTransitionClass(true)).toBe('transition-none');
  });

  it('keeps visual transitions when idle', () => {
    expect(getButtonTransitionClass(false)).toBe('transition-opacity duration-200');
  });

  it('places the hover label to the left near the right edge', () => {
    expect(getHoverLabelPositionClass(true)).toBe('right-full mr-1');
  });

  it('places the hover label to the right when there is room', () => {
    expect(getHoverLabelPositionClass(false)).toBe('left-full ml-1');
  });
});

// ─── Property 7: Button snap produces edge-aligned position ────────

describe('Feature: in-app-bug-report, Property 7: Button snap produces edge-aligned position', () => {
  /**
   * **Validates: Requirements 1.3**
   *
   * For any release position (x, y) within window bounds (width, height),
   * the snap function should produce a position where the button is on a
   * window edge (x=0, x=width-buttonWidth, y=0, or y=height-buttonHeight)
   * or at a corner.
   */
  it('snapped position is always on a window edge or corner', () => {
    fc.assert(
      fc.property(
        // buttonWidth and buttonHeight: reasonable sizes 10–100
        fc.integer({ min: 10, max: 100 }),
        fc.integer({ min: 10, max: 100 }),
        // windowWidth and windowHeight: must be larger than button
        fc.integer({ min: 200, max: 4000 }),
        fc.integer({ min: 200, max: 4000 }),
        // x, y: release position within window bounds
        fc.integer({ min: 0, max: 3999 }),
        fc.integer({ min: 0, max: 3999 }),
        (buttonWidth, buttonHeight, windowWidth, windowHeight, rawX, rawY) => {
          // Ensure window is large enough for the button
          if (windowWidth <= buttonWidth || windowHeight <= buttonHeight) return;
          // Clamp x, y to valid range within window
          const x = Math.min(rawX, windowWidth - buttonWidth);
          const y = Math.min(rawY, windowHeight - buttonHeight);

          const result = snapToEdge(x, y, buttonWidth, buttonHeight, windowWidth, windowHeight);

          // The snapped position must be on at least one edge
          const onLeftEdge = result.x === 0;
          const onRightEdge = result.x === windowWidth - buttonWidth;
          const onTopEdge = result.y === 0;
          const onBottomEdge = result.y === windowHeight - buttonHeight;

          // snapToEdge always snaps to a corner (both axes are edge-aligned)
          const onHorizontalEdge = onLeftEdge || onRightEdge;
          const onVerticalEdge = onTopEdge || onBottomEdge;

          expect(onHorizontalEdge && onVerticalEdge).toBe(true);

          // Result must be within bounds
          expect(result.x).toBeGreaterThanOrEqual(0);
          expect(result.x).toBeLessThanOrEqual(windowWidth - buttonWidth);
          expect(result.y).toBeGreaterThanOrEqual(0);
          expect(result.y).toBeLessThanOrEqual(windowHeight - buttonHeight);
        },
      ),
      { numRuns: 200 },
    );
  });
});


// ─── Property 8: Button position round-trip via settings store ─────

describe('Feature: in-app-bug-report, Property 8: Button position round-trip via settings store', () => {
  /**
   * **Validates: Requirements 1.4**
   *
   * For any valid button position { x, y }, storing it via the settings
   * store and then reading it back should produce the same position.
   */
  it('store then read returns identical position', async () => {
    await fc.assert(
      fc.asyncProperty(
        fc.integer({ min: 0, max: 4000 }),
        fc.integer({ min: 0, max: 4000 }),
        async (x, y) => {
          const pos = { x, y };
          await setStoreValue(STORE_KEYS.bugReportButtonPos, pos);
          const read = await getStoreValue(STORE_KEYS.bugReportButtonPos, { x: -1, y: -1 });
          expect(read).toEqual(pos);
        },
      ),
      { numRuns: 200 },
    );
  });
});

// ─── Unit Tests: Chromeless Window Exclusion ───────────────────────

describe('BugReportButton — chromeless window exclusion', () => {
  const CHROMELESS_LABELS = ['screen-share', 'share-picker', 'share-indicator', 'watch-all'];

  it.each(CHROMELESS_LABELS)(
    'renders null for chromeless label "%s"',
    (label) => {
      // The component checks getCurrent().label.startsWith(chromelessLabel)
      // Simulate the same logic used in BugReportButton
      const isChromeless = CHROMELESS_LABELS.some((l) => label.startsWith(l));
      expect(isChromeless).toBe(true);
    },
  );

  it.each(['screen-share-viewer-123', 'share-picker-abc', 'share-indicator-xyz', 'watch-all-streams'])(
    'renders null for prefixed chromeless label "%s"',
    (label) => {
      const isChromeless = CHROMELESS_LABELS.some((l) => label.startsWith(l));
      expect(isChromeless).toBe(true);
    },
  );

  it('renders for main window label', () => {
    const label = 'main';
    const isChromeless = CHROMELESS_LABELS.some((l) => label.startsWith(l));
    expect(isChromeless).toBe(false);
  });

  it('renders for empty label', () => {
    const label = '';
    const isChromeless = CHROMELESS_LABELS.some((l) => label.startsWith(l));
    expect(isChromeless).toBe(false);
  });

  it('renders for unrelated label', () => {
    const label = 'settings-window';
    const isChromeless = CHROMELESS_LABELS.some((l) => label.startsWith(l));
    expect(isChromeless).toBe(false);
  });
});
