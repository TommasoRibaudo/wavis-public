/**
 * Room navigation guard.
 *
 * Keeps the active room mounted against accidental navigation (router
 * blocker + hardware back-gesture suppression), owns the one sanctioned
 * exit path (navigateAwayFromRoom), redirects home when no channel is
 * selected, navigates out after a self-kick, and clears the persisted
 * last-channel when it is no longer safe to auto-join.
 *
 * Decision logic lives in @shared/helpers (shouldBlockRoomNavigation,
 * shouldPreventRoomNavigationGesture) — this hook is wiring only.
 */

import { useCallback, useEffect, useRef, type MutableRefObject } from 'react';
import { useBlocker, useNavigate } from 'react-router';
import { shouldBlockRoomNavigation, shouldPreventRoomNavigationGesture } from '@shared/helpers';
import { clearLastChannel } from '@features/settings/settings-store';

const NOT_AUTHORIZED_REJECTION_PREFIX = 'Unable to join voice.';
const SELF_KICK_REDIRECT_DELAY_MS = 2_000;

interface UseRoomNavigationGuardOptions {
  channelId: string | undefined;
  error: string | null | undefined;
  rejectionReason: string | null | undefined;
}

interface RoomNavigationGuard {
  /** Navigate out of the room through the guard (unblocks the router). */
  navigateAwayFromRoom: (target: string, leaveMode?: 'none' | 'immediate') => void;
  /** True once navigation out of the room is sanctioned. */
  allowNavigationRef: MutableRefObject<boolean>;
  /** True when the unmount cleanup must NOT call leaveRoom(). */
  skipUnmountLeaveRef: MutableRefObject<boolean>;
}

export function useRoomNavigationGuard({
  channelId,
  error,
  rejectionReason,
}: UseRoomNavigationGuardOptions): RoomNavigationGuard {
  const navigate = useNavigate();
  const allowNavigationRef = useRef(false);
  const skipUnmountLeaveRef = useRef(false);

  const blocker = useBlocker(({ currentLocation, nextLocation }) =>
    shouldBlockRoomNavigation(
      currentLocation.pathname,
      nextLocation.pathname,
      allowNavigationRef.current,
    ),
  );

  const navigateAwayFromRoom = useCallback(
    (target: string, leaveMode: 'none' | 'immediate' = 'none') => {
      allowNavigationRef.current = true;
      // skipUnmountLeaveRef stays false so the unmount cleanup calls leaveRoom().
      // We do NOT call leaveRoom() here: calling it before navigate() causes
      // notify() to re-render ActiveRoom in idle state before the route changes,
      // making the lower leave button flash on screen for one frame.
      skipUnmountLeaveRef.current = false;
      navigate(
        target,
        leaveMode === 'immediate' ? { state: { skipAutoRedirect: true } } : undefined,
      );
    },
    [navigate],
  );

  // Guard: no channelId → redirect home
  useEffect(() => {
    if (!channelId) {
      allowNavigationRef.current = true;
      navigate('/');
    }
  }, [channelId, navigate]);

  // Keep the room mounted unless the app explicitly allows navigation out.
  useEffect(() => {
    if (blocker.state === 'blocked') {
      blocker.reset();
    }
  }, [blocker]);

  // Suppress hardware/browser back gestures before they trigger navigation.
  useEffect(() => {
    const onMouseNavigation = (event: MouseEvent) => {
      if (!shouldPreventRoomNavigationGesture({ button: event.button })) return;
      event.preventDefault();
      event.stopPropagation();
    };

    const onKeyNavigation = (event: KeyboardEvent) => {
      if (!shouldPreventRoomNavigationGesture({ key: event.key, altKey: event.altKey })) return;
      event.preventDefault();
      event.stopPropagation();
    };

    window.addEventListener('mousedown', onMouseNavigation, true);
    window.addEventListener('mouseup', onMouseNavigation, true);
    window.addEventListener('auxclick', onMouseNavigation, true);
    window.addEventListener('keydown', onKeyNavigation, true);
    return () => {
      window.removeEventListener('mousedown', onMouseNavigation, true);
      window.removeEventListener('mouseup', onMouseNavigation, true);
      window.removeEventListener('auxclick', onMouseNavigation, true);
      window.removeEventListener('keydown', onKeyNavigation, true);
    };
  }, []);

  // Clear persisted last channel when the saved channel is no longer safe to auto-join.
  useEffect(() => {
    if (
      error === 'You were kicked' ||
      rejectionReason?.startsWith(NOT_AUTHORIZED_REJECTION_PREFIX)
    ) {
      void clearLastChannel();
    }
  }, [error, rejectionReason]);

  // Self-kick navigation
  useEffect(() => {
    if (error === 'You were kicked') {
      const timer = setTimeout(
        () => navigateAwayFromRoom(`/channel/${channelId}`),
        SELF_KICK_REDIRECT_DELAY_MS,
      );
      return () => clearTimeout(timer);
    }
  }, [error, channelId, navigateAwayFromRoom]);

  return { navigateAwayFromRoom, allowNavigationRef, skipUnmountLeaveRef };
}
