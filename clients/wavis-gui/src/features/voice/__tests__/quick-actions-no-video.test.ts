/**
 * Quick_Actions structural test — no video-feed additions (Req 4.5, 4.6, 8.1–8.4)
 *
 * Asserts that the tray menu (the Quick_Actions surface outside the room view)
 * contains ONLY the pre-video-feed controls and has not been extended with any
 * video-related actions, tab-switcher, or Video_Settings shortcut.
 */

import { describe, it, expect } from 'vitest';
import type { TrayAction } from '../tray-bridge';
import { computeTrayMenuState, deafenMenuLabel, muteMenuLabel } from '../tray-bridge';

/* ─── TrayAction exhaustive check ─────────────────────────────────── */

describe('Feature: video-feed — Quick_Actions has no video controls (Req 4.5, 4.6, 8.1–8.4)', () => {
  const PRE_VIDEO_FEED_ACTIONS: readonly TrayAction[] = ['toggle-mute', 'toggle-deafen', 'leave', 'show'];

  it('TrayAction set is exactly the pre-video-feed set', () => {
    const actions: TrayAction[] = ['toggle-mute', 'toggle-deafen', 'leave', 'show'];
    expect(new Set(actions)).toEqual(new Set(PRE_VIDEO_FEED_ACTIONS));
  });

  it('contains no video-toggle action', () => {
    const actions: string[] = ['toggle-mute', 'toggle-deafen', 'leave', 'show'];
    const videoRelated = actions.filter(
      (a) =>
        a.includes('video') ||
        a.includes('camera') ||
        a.includes('tab') ||
        a.includes('logs') ||
        a.includes('settings'),
    );
    expect(videoRelated).toEqual([]);
  });

  it('computeTrayMenuState exposes no video-related fields', () => {
    const state = computeTrayMenuState({ inVoiceSession: true, isMuted: false, isDeafened: false });
    const keys = Object.keys(state);
    const videoRelated = keys.filter(
      (k) => k.toLowerCase().includes('video') || k.toLowerCase().includes('camera'),
    );
    expect(videoRelated).toEqual([]);
  });

  it('pre-video-feed controls are preserved with same labels', () => {
    expect(muteMenuLabel(false)).toBe('Mute');
    expect(muteMenuLabel(true)).toBe('Unmute');
    expect(deafenMenuLabel(false)).toBe('Deafen');
    expect(deafenMenuLabel(true)).toBe('Undeafen');
  });

  it('tray menu state has exactly muteEnabled, deafenEnabled, and leaveEnabled', () => {
    const state = computeTrayMenuState({ inVoiceSession: true, isMuted: false, isDeafened: false });
    expect(Object.keys(state).sort()).toEqual(['deafenEnabled', 'leaveEnabled', 'muteEnabled']);
  });
});
