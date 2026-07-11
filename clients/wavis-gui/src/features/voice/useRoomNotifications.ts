/**
 * Room event notifications.
 *
 * Watches the room event log and surfaces new events as sonner toasts and
 * native notifications (both gated by the user's notification toggles).
 * Message/color/toggle policy lives in @shared/helpers — this hook is wiring.
 */

import { useCallback, useEffect, useRef } from 'react';
import { toast } from 'sonner';
import { toastMessageForEvent, toastColorForEvent, eventToToggleKey } from '@shared/helpers';
import { isNotificationEnabled } from '@features/settings/settings-store';
import { sendWavisNotification } from '@shared/notification-bridge';
import type { RoomEvent } from './chat-display-model';

const TOAST_STYLE_BASE = {
  fontFamily: 'var(--font-mono)',
  fontSize: '0.875rem',
} as const;

export function useRoomNotifications(events: RoomEvent[] | undefined): {
  /** Forget the seen-events cursor (call when a session ends/restarts). */
  resetNotificationCursor: () => void;
} {
  const prevEventsLenRef = useRef(0);

  useEffect(() => {
    if (!events) return;
    const prevLen = prevEventsLenRef.current;
    prevEventsLenRef.current = events.length;
    if (prevLen === 0 || events.length <= prevLen) return;
    const newEvents = events.slice(prevLen);
    for (const ev of newEvents) {
      if (ev.shouldToast === false) continue;
      const name = ev.message.split(' ')[0] ?? '';
      const msg = toastMessageForEvent(ev.type, name);
      if (!msg) continue;
      const toggleKey = eventToToggleKey(ev.type);
      const showToast = () => {
        toast(msg, {
          style: { borderLeft: `3px solid ${toastColorForEvent(ev.type)}`, ...TOAST_STYLE_BASE },
        });
      };
      if (toggleKey) {
        void isNotificationEnabled(toggleKey).then((enabled) => {
          if (enabled) showToast();
        });
        // Also send native notification (gated by visibility + toggle inside sendWavisNotification)
        sendWavisNotification(toggleKey, msg);
      } else {
        showToast();
      }
    }
  }, [events]);

  const resetNotificationCursor = useCallback(() => {
    prevEventsLenRef.current = 0;
  }, []);

  return { resetNotificationCursor };
}
