import { describe, it, expect, vi, beforeEach } from 'vitest';
import fc from 'fast-check';
import type { NotificationToggles } from '@features/settings/settings-store';

/* ─── Mock setup ────────────────────────────────────────────────── */

// Mock the Tauri store plugin at the lowest level
const mockStoreData = new Map<string, unknown>();
const mockStoreInstance = {
  get: vi.fn(async (key: string) => mockStoreData.get(key) ?? null),
  set: vi.fn(async (key: string, value: unknown) => { mockStoreData.set(key, value); }),
  save: vi.fn(async () => {}),
};

vi.mock('@tauri-apps/plugin-store', () => ({
  load: vi.fn(async () => mockStoreInstance),
}));

// Import after mocks
const {
  isNotificationEnabled,
  setNotificationToggle,
} = await import('@features/settings/settings-store');

beforeEach(() => {
  mockStoreData.clear();
  vi.clearAllMocks();
});

/* ─── Property 5: Notification suppression when toggle disabled ── */
// Feature: gui-feature-completion, Property 5
// **Validates: Requirements 13.5, 13.6, 19.6**

const TOGGLE_KEYS: Array<keyof NotificationToggles> = [
  'participantJoined',
  'participantLeft',
  'participantKicked',
  'participantMutedByHost',
  'inviteReceived',
];

describe('Property 5: Notification suppression when toggle disabled', () => {
  it('for any event type with toggle set to false, isNotificationEnabled returns false', () => {
    return fc.assert(
      fc.asyncProperty(
        fc.constantFrom(...TOGGLE_KEYS),
        async (key) => {
          mockStoreData.clear();
          await setNotificationToggle(key, false);
          const enabled = await isNotificationEnabled(key);
          expect(enabled).toBe(false);
        },
      ),
      { numRuns: 50 },
    );
  });

  it('for any event type with toggle set to true, isNotificationEnabled returns true', () => {
    return fc.assert(
      fc.asyncProperty(
        fc.constantFrom(...TOGGLE_KEYS),
        async (key) => {
          mockStoreData.clear();
          await setNotificationToggle(key, true);
          const enabled = await isNotificationEnabled(key);
          expect(enabled).toBe(true);
        },
      ),
      { numRuns: 50 },
    );
  });

  it('all toggles default to true (enabled) when no value is set', () => {
    return fc.assert(
      fc.asyncProperty(
        fc.constantFrom(...TOGGLE_KEYS),
        async (key) => {
          mockStoreData.clear();
          const enabled = await isNotificationEnabled(key);
          expect(enabled).toBe(true);
        },
      ),
      { numRuns: 25 },
    );
  });

  it('toggling off then on restores enabled state', () => {
    return fc.assert(
      fc.asyncProperty(
        fc.constantFrom(...TOGGLE_KEYS),
        async (key) => {
          mockStoreData.clear();
          await setNotificationToggle(key, false);
          expect(await isNotificationEnabled(key)).toBe(false);
          await setNotificationToggle(key, true);
          expect(await isNotificationEnabled(key)).toBe(true);
        },
      ),
      { numRuns: 25 },
    );
  });
});


/* ─── Property 8: Tray notifications sent only when window is hidden ── */
// Feature: gui-feature-completion, Property 8
// **Validates: Requirements 19.1, 19.2, 19.7**

// We need to test the notification-bridge module with controlled visibility state.
// Since windowVisible is module-level, we test via initNotificationBridge + event simulation.

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === 'is_window_visible') return true;
    return null;
  }),
}));

vi.mock('@tauri-apps/api/event', () => {
  const listeners = new Map<string, Array<(event: unknown) => void>>();
  return {
    listen: vi.fn(async (eventName: string, handler: (event: unknown) => void) => {
      if (!listeners.has(eventName)) listeners.set(eventName, []);
      listeners.get(eventName)!.push(handler);
      return () => {
        const arr = listeners.get(eventName);
        if (arr) {
          const idx = arr.indexOf(handler);
          if (idx >= 0) arr.splice(idx, 1);
        }
      };
    }),
    emit: vi.fn(async () => {}),
    __listeners: listeners,
  };
});

const mockSendNotification = vi.fn();
const mockIsPermissionGranted = vi.fn(async () => true);
const mockRequestPermission = vi.fn(async () => 'granted' as const);

vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: () => mockIsPermissionGranted(),
  requestPermission: () => mockRequestPermission(),
  sendNotification: (...args: unknown[]) => mockSendNotification(args[0]),
}));

const notifBridge = await import('../notification-bridge');
const eventModule = await import('@tauri-apps/api/event');

describe('Property 8: Tray notifications sent only when window is hidden', () => {
  beforeEach(() => {
    mockSendNotification.mockClear();
    mockIsPermissionGranted.mockResolvedValue(true);
    mockStoreData.clear();
  });

  it('notification NOT sent when window is visible', async () => {
    // Init seeds windowVisible = true (from mock invoke)
    notifBridge.initNotificationBridge();
    await new Promise((r) => setTimeout(r, 10)); // let init settle

    await notifBridge.sendWavisNotification('participantJoined', 'Alice joined');
    expect(mockSendNotification).not.toHaveBeenCalled();
  });

  it('notification sent when window is hidden', async () => {
    notifBridge.initNotificationBridge();
    await new Promise((r) => setTimeout(r, 10));

    // Simulate window hidden event
    const listeners = (eventModule as unknown as { __listeners: Map<string, Array<(e: unknown) => void>> }).__listeners;
    const visListeners = listeners.get('window-visibility-changed') ?? [];
    for (const fn of visListeners) {
      fn({ payload: { visible: false } });
    }

    await notifBridge.sendWavisNotification('participantJoined', 'Alice joined');
    expect(mockSendNotification).toHaveBeenCalledWith({ title: 'Wavis', body: 'Alice joined' });
  });

  it('notification suppressed when toggle is disabled even if window hidden', async () => {
    notifBridge.initNotificationBridge();
    await new Promise((r) => setTimeout(r, 10));

    // Hide window
    const listeners = (eventModule as unknown as { __listeners: Map<string, Array<(e: unknown) => void>> }).__listeners;
    const visListeners = listeners.get('window-visibility-changed') ?? [];
    for (const fn of visListeners) {
      fn({ payload: { visible: false } });
    }

    // Disable the toggle
    await setNotificationToggle('participantJoined', false);

    await notifBridge.sendWavisNotification('participantJoined', 'Alice joined');
    expect(mockSendNotification).not.toHaveBeenCalled();
  });
});
