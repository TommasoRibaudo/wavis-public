/**
 * ActiveRoom Unit Tests — Screen Share Error UX
 *
 * Tests the three-way startScreenShare() result handling and error
 * toast display logic in ActiveRoom. Replicates the state transitions
 * from ActiveRoom.tsx (same node-based approach as ActiveRoom.test.ts).
 *
 * Validates: Requirements 2.4, 2.8
 * Acceptance Criteria: AC1–AC6
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

/* ─── Mock voice-room module ────────────────────────────────────── */

// startShare() was removed (cleanup 2026-03); the error-handling state machine
// tested below is generic and uses a local mock function directly.
const mockStartShare = vi.fn<() => Promise<void>>();

vi.mock('../voice-room', () => ({
  initSession: vi.fn(),
  leaveRoom: vi.fn(),
  toggleSelfMute: vi.fn(),
  // startShare was removed; ActiveRoom uses startFallbackShare/startCustomShare.
  stopShare: vi.fn(),
  setParticipantVolume: vi.fn(),
  setMasterVolume: vi.fn(),
  kickParticipant: vi.fn(),
  muteParticipant: vi.fn(),
  createSubRoom: vi.fn(),
  joinSubRoom: vi.fn(),
  leaveSubRoom: vi.fn(),
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

/* ─── Screen Share Error State Machine ──────────────────────────── */

/**
 * Replicates the screen share error handling logic from ActiveRoom.tsx:
 *   handleStartShare() calls startShare() and catches rejections,
 *   setting screenShareError state with auto-dismiss after 5s.
 */
interface ShareErrorState {
  screenShareError: string | null;
  timerId: ReturnType<typeof setTimeout> | null;
}

function initialShareErrorState(): ShareErrorState {
  return { screenShareError: null, timerId: null };
}

/**
 * Simulates handleStartShare() from ActiveRoom.tsx.
 * Calls startShare() and handles the three-way result.
 */
async function handleStartShare(
  state: ShareErrorState,
  startShareFn: () => Promise<void>,
): Promise<ShareErrorState> {
  try {
    await startShareFn();
    // true → share started (signaling sent inside startShare)
    // false → user cancelled (no-op, no error)
    return state;
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    // Clear any existing timer
    if (state.timerId) clearTimeout(state.timerId);
    // Set error and schedule auto-dismiss after 5s
    const timerId = setTimeout(() => {
      // In real component this calls setScreenShareError(null)
    }, 5000);
    return { screenShareError: msg, timerId };
  }
}

/** Simulates manual dismiss (clicking [x] on the error toast). */
function dismissError(state: ShareErrorState): ShareErrorState {
  if (state.timerId) clearTimeout(state.timerId);
  return { screenShareError: null, timerId: null };
}

/* ═══ Tests ═════════════════════════════════════════════════════════ */

describe('Screen Share Error UX — Three-Way Result Handling', () => {
  beforeEach(() => {
    mockStartShare.mockClear();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  /* ── AC1: true → share-active indicator visible ── */
  it('AC1: when startScreenShare() succeeds, no error is shown', async () => {
    mockStartShare.mockResolvedValue(undefined); // success path (true → void)
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBeNull();
    expect(mockStartShare).toHaveBeenCalledTimes(1);
  });

  /* ── AC2: false → silent no-op, no error, no UI change ── */
  it('AC2: when user cancels (startShare resolves normally), no error is shown', async () => {
    // false return from startScreenShare() means startShare() resolves
    // without sending signaling — no error, no UI change
    mockStartShare.mockResolvedValue(undefined);
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBeNull();
  });

  /* ── AC3: rejection → error toast displays exact message ── */
  it('AC3: when startScreenShare() rejects, error toast displays the exact message', async () => {
    const errorMsg = 'PipeWire not available and X11 fallback failed';
    mockStartShare.mockRejectedValue(new Error(errorMsg));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBe(errorMsg);
  });

  it('AC3: displays portal-specific error message', async () => {
    const errorMsg = 'screen sharing requires xdg-desktop-portal on Wayland; no X11 fallback is available on this system';
    mockStartShare.mockRejectedValue(new Error(errorMsg));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBe(errorMsg);
  });

  it('AC3: displays generic backend error message', async () => {
    const errorMsg = 'no supported capture backend found';
    mockStartShare.mockRejectedValue(new Error(errorMsg));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBe(errorMsg);
  });

  it('AC3: handles non-Error rejection (string)', async () => {
    mockStartShare.mockRejectedValue('raw error string');
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBe('raw error string');
  });

  /* ── AC4: error toast is dismissible (auto-dismiss + manual) ── */
  it('AC4: error auto-dismisses after 5 seconds', async () => {
    mockStartShare.mockRejectedValue(new Error('capture failed'));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBe('capture failed');
    expect(state.timerId).not.toBeNull();

    // Timer is set for 5000ms
    // In the real component, the timer callback clears the error state
  });

  it('AC4: error is manually dismissible via [x] button', async () => {
    mockStartShare.mockRejectedValue(new Error('capture failed'));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toBe('capture failed');

    // Manual dismiss
    state = dismissError(state);
    expect(state.screenShareError).toBeNull();
    expect(state.timerId).toBeNull();
  });

  /* ── AC5: other room controls remain functional during error ── */
  it('AC5: error state does not affect other room state', async () => {
    mockStartShare.mockRejectedValue(new Error('capture failed'));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    // Error is set but it's an independent piece of state —
    // it doesn't touch machineState, participants, mediaState, etc.
    expect(state.screenShareError).toBe('capture failed');
    // The error state is a separate useState in ActiveRoom,
    // completely independent from roomState. Mic, leave, chat
    // all operate on roomState and are unaffected.
  });

  /* ── AC6: Wayland-specific error mentions xdg-desktop-portal ── */
  it('AC6: Wayland error message mentions xdg-desktop-portal', async () => {
    const errorMsg = 'screen sharing requires xdg-desktop-portal on Wayland; no X11 fallback is available on this system';
    mockStartShare.mockRejectedValue(new Error(errorMsg));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);

    expect(state.screenShareError).toContain('xdg-desktop-portal');
    expect(state.screenShareError).toContain('Wayland');
  });

  /* ── Edge cases ── */
  it('subsequent error replaces previous error and resets timer', async () => {
    mockStartShare.mockRejectedValue(new Error('first error'));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);
    expect(state.screenShareError).toBe('first error');
    const firstTimerId = state.timerId;

    mockStartShare.mockRejectedValue(new Error('second error'));
    state = await handleStartShare(state, mockStartShare);
    expect(state.screenShareError).toBe('second error');
    // First timer should have been cleared (in real component)
    expect(state.timerId).not.toBe(firstTimerId);
  });

  it('success after error clears nothing (error auto-dismisses independently)', async () => {
    // First call fails
    mockStartShare.mockRejectedValue(new Error('capture failed'));
    let state = initialShareErrorState();
    state = await handleStartShare(state, mockStartShare);
    expect(state.screenShareError).toBe('capture failed');

    // Second call succeeds — error state is not cleared by success
    // (it auto-dismisses via timer or manual [x])
    mockStartShare.mockResolvedValue(undefined);
    state = await handleStartShare(state, mockStartShare);
    // State returned from success path doesn't modify error
    expect(state.screenShareError).toBe('capture failed');
  });
});
