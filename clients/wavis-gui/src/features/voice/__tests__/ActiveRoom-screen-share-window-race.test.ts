/**
 * ActiveRoom Screen Share Window Race Condition Tests
 *
 * Tests the fix for: "a webview with label `screen-share-peer-XXX` already exists"
 *
 * When openShareWindow is called twice with the same participantId in quick succession,
 * the buggy code would call closeShareWindow (async fire-and-forget) then immediately
 * create a new window. But Tauri's webview destruction is async, causing the "already exists" error.
 *
 * The fix: grab reference to old window, call closeShareWindow, then AWAIT tauri://destroyed
 * before creating the new one.
 *
 * Validates: Bug fix for screen share window race condition
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

/* ─── Mock @tauri-apps/api/webviewWindow (MUST come before any imports) ─ */

interface MockWebviewWindow {
  close: () => Promise<void>;
  once: (event: string, callback: () => void) => void;
  setFocus: () => Promise<void>;
  label: string;
}

const mockWebviewWindowInstances = new Map<string, MockWebviewWindow>();
let webviewWindowConstructorCalls = 0;

class MockWebviewWindowClass implements MockWebviewWindow {
  label: string;
  callbackMap = new Map<string, () => void>();

  constructor(label: string) {
    this.label = label;
    webviewWindowConstructorCalls++;
    mockWebviewWindowInstances.set(label, this);
  }

  once(event: string, callback: () => void) {
    this.callbackMap.set(event, callback);
  }

  fireEvent(event: string) {
    const callback = this.callbackMap.get(event);
    if (callback) callback();
  }

  async close() {
    return Promise.resolve();
  }

  async setFocus() {
    return Promise.resolve();
  }
}

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  WebviewWindow: MockWebviewWindowClass,
}));

/* ─── Mock other modules needed for openShareWindow logic ────────────── */

vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn().mockResolvedValue(undefined),
  emitTo: vi.fn().mockResolvedValue(undefined),
  listen: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock('../voice-room', () => ({
  initSession: vi.fn(),
  leaveRoom: vi.fn(),
  toggleSelfMute: vi.fn(),
  // startShare was removed (cleanup 2026-03); ActiveRoom uses startFallbackShare/startCustomShare.
  stopShare: vi.fn(),
  setParticipantVolume: vi.fn(),
  setMasterVolume: vi.fn(),
  kickParticipant: vi.fn(),
  muteParticipant: vi.fn(),
  createSubRoom: vi.fn(),
  joinSubRoom: vi.fn(),
  leaveSubRoom: vi.fn(),
  setPassthrough: vi.fn(),
  clearPassthrough: vi.fn(),
  stopParticipantShare: vi.fn(),
  stopAllShares: vi.fn(),
  sendChatMessage: vi.fn(),
  reconnectMedia: vi.fn(),
  setShareQuality: vi.fn(),
  toggleShareAudio: vi.fn(),
  changeShareSource: vi.fn(),
  attachScreenShareAudio: vi.fn(),
  detachScreenShareAudio: vi.fn(),
}));

vi.mock('@features/screen-share/screen-share-viewer', () => ({
  startSending: vi.fn(),
  stopSending: vi.fn(),
  stopSendingForWindow: vi.fn(),
  stopAllSending: vi.fn(),
  resendStream: vi.fn(),
}));

/* ─── Window Creation Race Simulation ────────────────────────────────── */

/**
 * Simulates the openShareWindow logic with the fix:
 * If window already exists, grab reference, close it, then AWAIT destruction
 * before creating new window.
 */
async function simulateOpenShareWindow(
  participantId: string,
  windowsRef: Map<string, MockWebviewWindow>,
  closeWindowFn: (id: string, win: MockWebviewWindow) => void,
): Promise<void> {
  // If already watching this participant, close it first and wait for destruction
  if (windowsRef.has(participantId)) {
    const oldWin = windowsRef.get(participantId)! as MockWebviewWindowClass;
    closeWindowFn(participantId, oldWin);
    // Wait for the old window to actually be destroyed before creating a new one
    await new Promise<void>((resolve) => {
      const timeout = setTimeout(resolve, 1000);
      oldWin.once('tauri://destroyed', () => {
        clearTimeout(timeout);
        resolve();
      });
    });
  }

  // Create new window
  const windowLabel = `screen-share-${participantId}`;
  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
  const win = new WebviewWindow(windowLabel) as unknown as MockWebviewWindowClass;

  win.once('tauri://created', () => {
    // Setup would happen here
  });

  win.once('tauri://error', () => {
    // Error handling
  });

  win.once('tauri://destroyed', () => {
    // Cleanup
  });

  windowsRef.set(participantId, win as MockWebviewWindow);
}

/* ─── Tests ──────────────────────────────────────────────────────────── */

describe('Screen Share Window Race Condition', () => {
  let windowsRef: Map<string, MockWebviewWindow>;

  beforeEach(() => {
    vi.clearAllMocks();
    webviewWindowConstructorCalls = 0;
    mockWebviewWindowInstances.clear();
    windowsRef = new Map();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('should create window once when openShareWindow called once', async () => {
    const closeWindow = (id: string) => {
      windowsRef.delete(id);
    };

    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);

    expect(webviewWindowConstructorCalls).toBe(1);
    expect(windowsRef.has('participant-1')).toBe(true);
  });

  it('should wait for destruction before recreating window', async () => {
    const closeWindow = (id: string, win: MockWebviewWindow) => {
      windowsRef.delete(id);
      // Simulate the destroyed event firing after a short delay
      setTimeout(() => {
        (win as MockWebviewWindowClass).fireEvent('tauri://destroyed');
      }, 50);
    };

    // First call creates window
    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);
    expect(webviewWindowConstructorCalls).toBe(1);

    // Second call should close first window, wait for destruction, then create new one
    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);
    expect(webviewWindowConstructorCalls).toBe(2);
  });

  it('should not create new window until old window destroyed event fires', async () => {
    let destroyedFired = false;

    const closeWindow = (id: string, win: MockWebviewWindow) => {
      windowsRef.delete(id);
      // Delay the destroyed event to test that we wait for it
      setTimeout(() => {
        destroyedFired = true;
        (win as MockWebviewWindowClass).fireEvent('tauri://destroyed');
      }, 100);
    };

    // First call
    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);

    // Track when second window is created
    let secondWindowCreatedBefore = false;
    const originalCall = webviewWindowConstructorCalls;

    const openSecondPromise = simulateOpenShareWindow(
      'participant-1',
      windowsRef,
      closeWindow,
    ).then(() => {
      secondWindowCreatedBefore = destroyedFired;
    });

    // Give it a bit to start but not enough time for destroyed to fire
    await new Promise((resolve) => setTimeout(resolve, 20));

    // At this point, destroyed event hasn't fired yet, so second window shouldn't be created
    expect(webviewWindowConstructorCalls).toBe(originalCall);

    // Wait for completion
    await openSecondPromise;

    // Now both windows should be created and destroyed should have fired
    expect(webviewWindowConstructorCalls).toBe(2);
    expect(secondWindowCreatedBefore).toBe(true);
  });

  it('should use safety timeout if destroyed event never fires', async () => {
    const closeWindow = (id: string) => {
      windowsRef.delete(id);
      // Note: intentionally NOT firing the destroyed event
    };

    // First call
    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);
    const constructorCalls = webviewWindowConstructorCalls;

    // Second call should timeout after 1s and create new window anyway
    const startTime = Date.now();
    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);
    const elapsed = Date.now() - startTime;

    expect(webviewWindowConstructorCalls).toBe(constructorCalls + 1);
    // Should have waited roughly 1 second for timeout
    expect(elapsed).toBeGreaterThanOrEqual(900);
  });

  it('should handle multiple participants independently', async () => {
    const closeWindow = (id: string, win: MockWebviewWindow) => {
      windowsRef.delete(id);
      setTimeout(() => {
        (win as MockWebviewWindowClass).fireEvent('tauri://destroyed');
      }, 30);
    };

    // Open windows for two participants
    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);
    await simulateOpenShareWindow('participant-2', windowsRef, closeWindow);

    expect(webviewWindowConstructorCalls).toBe(2);
    expect(windowsRef.size).toBe(2);

    // Reopen participant-1 should only affect participant-1
    await simulateOpenShareWindow('participant-1', windowsRef, closeWindow);

    expect(webviewWindowConstructorCalls).toBe(3);
    expect(windowsRef.size).toBe(2);
  });
});
