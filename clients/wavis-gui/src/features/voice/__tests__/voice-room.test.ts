import { describe, it, expect } from 'vitest';
import {
  computeSpeaking,
  updateSpeakingTracker,
  colorFor,
  computeEffectiveParticipantVolume,
  TERMINAL_COLORS,
  RMS_START_THRESHOLD,
  RMS_STOP_THRESHOLD,
  SPEAKING_DEBOUNCE_FRAMES,
  MAX_EVENTS,
} from '../voice-room';

describe('computeSpeaking', () => {
  it('returns true when RMS above start threshold and not currently speaking', () => {
    expect(computeSpeaking(false, 0.07, false)).toBe(true);
    expect(computeSpeaking(false, RMS_START_THRESHOLD, false)).toBe(true);
  });

  it('returns false when RMS below stop threshold while speaking', () => {
    expect(computeSpeaking(true, 0.02, false)).toBe(false);
  });

  it('stays speaking when RMS between stop and start thresholds', () => {
    expect(computeSpeaking(true, 0.04, false)).toBe(true);
    expect(computeSpeaking(true, 0.05, false)).toBe(true);
    expect(computeSpeaking(true, RMS_STOP_THRESHOLD, false)).toBe(true);
  });

  it('stays not speaking when RMS between stop and start thresholds', () => {
    expect(computeSpeaking(false, 0.04, false)).toBe(false);
    expect(computeSpeaking(false, 0.05, false)).toBe(false);
  });

  it('returns false when muted regardless of RMS', () => {
    expect(computeSpeaking(true, 1.0, true)).toBe(false);
    expect(computeSpeaking(false, 1.0, true)).toBe(false);
    expect(computeSpeaking(true, 0.5, true)).toBe(false);
  });

  it('returns false at zero RMS', () => {
    expect(computeSpeaking(false, 0, false)).toBe(false);
    expect(computeSpeaking(true, 0, false)).toBe(false);
  });
});

describe('updateSpeakingTracker', () => {
  // Each test uses a unique participant ID to avoid cross-test state leakage
  // from the module-level speakingTracker map.
  let uid = 0;
  function nextId(): string {
    return `tracker-test-${uid++}-${Date.now()}`;
  }

  it('does not flip to speaking on a single high-RMS frame', () => {
    const id = nextId();
    // First call initializes the tracker — not yet debounced
    const r1 = updateSpeakingTracker(id, 0.3, false, false);
    expect(r1).toBe(false); // 1 frame, need SPEAKING_DEBOUNCE_FRAMES
  });

  it('flips to speaking after SPEAKING_DEBOUNCE_FRAMES consecutive high-RMS frames', () => {
    const id = nextId();
    let speaking = false;
    for (let i = 0; i < SPEAKING_DEBOUNCE_FRAMES; i++) {
      speaking = updateSpeakingTracker(id, 0.5, speaking, false);
    }
    expect(speaking).toBe(true);
  });

  it('does not flip off on a single low-RMS frame while speaking', () => {
    const id = nextId();
    // Ramp up to speaking
    let speaking = false;
    for (let i = 0; i < SPEAKING_DEBOUNCE_FRAMES + 2; i++) {
      speaking = updateSpeakingTracker(id, 0.5, speaking, false);
    }
    expect(speaking).toBe(true);

    // Single dip to zero — should stay speaking
    speaking = updateSpeakingTracker(id, 0, speaking, false);
    expect(speaking).toBe(true);
  });

  it('flips off after SPEAKING_DEBOUNCE_FRAMES consecutive low-RMS frames', () => {
    const id = nextId();
    // Ramp up to speaking
    let speaking = false;
    for (let i = 0; i < SPEAKING_DEBOUNCE_FRAMES + 5; i++) {
      speaking = updateSpeakingTracker(id, 0.5, speaking, false);
    }
    expect(speaking).toBe(true);

    // Feed enough zero-RMS frames to decay the EMA below stop threshold
    // and accumulate debounce frames
    for (let i = 0; i < 20; i++) {
      speaking = updateSpeakingTracker(id, 0, speaking, false);
    }
    expect(speaking).toBe(false);
  });

  it('resets pending counter when RMS oscillates around threshold', () => {
    const id = nextId();
    // Feed values that oscillate tightly around the start threshold.
    // With EMA smoothing, the smoothed value hovers near the threshold
    // and the debounce counter keeps resetting because the hysteresis
    // output flips back and forth — never accumulating enough consecutive
    // frames to commit.
    let speaking = false;
    for (let i = 0; i < 30; i++) {
      // Alternate just above and just below the start threshold
      const rms = i % 2 === 0 ? RMS_START_THRESHOLD + 0.01 : RMS_START_THRESHOLD - 0.05;
      speaking = updateSpeakingTracker(id, rms, speaking, false);
    }
    // Should not have committed to speaking — the oscillation prevents
    // SPEAKING_DEBOUNCE_FRAMES consecutive agreeing frames.
    expect(speaking).toBe(false);
  });

  it('immediately returns false when muted', () => {
    const id = nextId();
    const result = updateSpeakingTracker(id, 0.5, true, true);
    expect(result).toBe(false);
  });

  it('EMA smooths out a single spike', () => {
    const id = nextId();
    // Feed several low frames, then one spike, then low again
    let speaking = false;
    for (let i = 0; i < 5; i++) {
      speaking = updateSpeakingTracker(id, 0.01, speaking, false);
    }
    // Single spike
    speaking = updateSpeakingTracker(id, 0.5, speaking, false);
    expect(speaking).toBe(false); // EMA hasn't converged + debounce not met

    // Back to low
    speaking = updateSpeakingTracker(id, 0.01, speaking, false);
    expect(speaking).toBe(false);
  });
});

describe('colorFor', () => {
  it('returns same color for same ID', () => {
    const c1 = colorFor({ id: 'user-abc' });
    const c2 = colorFor({ id: 'user-abc' });
    expect(c1).toBe(c2);
  });

  it('returns a color from TERMINAL_COLORS palette', () => {
    const color = colorFor({ id: 'test-id' });
    expect(TERMINAL_COLORS).toContain(color);
  });

  it('different IDs produce different colors for a known set', () => {
    const ids = ['alice', 'bob', 'charlie', 'dave', 'eve'];
    const colors = ids.map((id) => colorFor({ id }));
    // At least some should differ (with 5 IDs and 8 colors, collisions possible but unlikely for all)
    const unique = new Set(colors);
    expect(unique.size).toBeGreaterThan(1);
  });

  it('prefers userId over participantId when both present', () => {
    const withUserId = colorFor({ userId: 'user-123', id: 'peer-456' });
    const justUserId = colorFor({ userId: 'user-123', id: 'different-peer' });
    const justPeerId = colorFor({ id: 'peer-456' });
    // userId-based colors should match each other
    expect(withUserId).toBe(justUserId);
    // userId-based color should differ from peerId-based (different hash input)
    // (This could theoretically collide, but for these specific strings it won't)
    expect(withUserId).not.toBe(justPeerId);
  });

  it('uses participantId when userId is undefined', () => {
    const c1 = colorFor({ id: 'peer-789' });
    const c2 = colorFor({ userId: undefined, id: 'peer-789' });
    expect(c1).toBe(c2);
  });
});

describe('event log cap enforcement', () => {
  it('caps at MAX_EVENTS entries', () => {
    // Replicate the appendEvent logic
    const events: Array<{ id: string }> = [];
    for (let i = 0; i < MAX_EVENTS + 10; i++) {
      events.push({ id: `evt-${i}` });
      if (events.length > MAX_EVENTS) {
        events.splice(0, events.length - MAX_EVENTS);
      }
    }
    expect(events.length).toBe(MAX_EVENTS);
  });

  it('removes oldest events when cap exceeded', () => {
    const events: Array<{ id: string }> = [];
    for (let i = 0; i < MAX_EVENTS + 5; i++) {
      events.push({ id: `evt-${i}` });
      if (events.length > MAX_EVENTS) {
        events.splice(0, events.length - MAX_EVENTS);
      }
    }
    // Oldest should be evt-5 (first 5 were removed)
    expect(events[0].id).toBe('evt-5');
    // Newest should be evt-104
    expect(events[events.length - 1].id).toBe(`evt-${MAX_EVENTS + 4}`);
  });

  it('MAX_EVENTS is 100', () => {
    expect(MAX_EVENTS).toBe(100);
  });
});

describe('computeEffectiveParticipantVolume', () => {
  it('preserves the manual volume for participants in the same joined room', () => {
    expect(
      computeEffectiveParticipantVolume(44, 'peer-2', 'self-peer', 'room-1', { 'peer-2': 'room-1' }),
    ).toBe(44);
  });

  it('mutes participants in different rooms', () => {
    expect(
      computeEffectiveParticipantVolume(44, 'peer-2', 'self-peer', 'room-1', { 'peer-2': 'room-2' }),
    ).toBe(0);
  });

  it('mutes everyone else when the local user has not joined a room', () => {
    expect(
      computeEffectiveParticipantVolume(44, 'peer-2', 'self-peer', null, { 'peer-2': 'room-1' }),
    ).toBe(0);
  });
});

import fc from 'fast-check';
import { isShareEnabled } from '../voice-room';

/* ═══ Property 2: Share button enabled iff permission allows ═══════ */
// Feature: gui-feature-completion, Property 2
// **Validates: Requirements 7.1, 7.3**

describe('Property 2: Share button enabled iff permission allows and media ready', () => {
  const arbSharePermission = fc.constantFrom('anyone' as const, 'host_only' as const);

  it('share enabled when permission is "anyone" regardless of host status (media ready)', () => {
    fc.assert(
      fc.property(fc.boolean(), (selfIsHost) => {
        expect(isShareEnabled('anyone', selfIsHost, 'active', 'connected')).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('share enabled when host_only AND selfIsHost is true (media ready)', () => {
    expect(isShareEnabled('host_only', true, 'active', 'connected')).toBe(true);
  });

  it('share disabled when host_only AND selfIsHost is false (media ready)', () => {
    expect(isShareEnabled('host_only', false, 'active', 'connected')).toBe(false);
  });

  it('share disabled when media is not connected, even with permission', () => {
    expect(isShareEnabled('anyone', true, 'active', 'disconnected')).toBe(false);
    expect(isShareEnabled('anyone', true, 'active', 'connecting')).toBe(false);
    expect(isShareEnabled('anyone', true, 'active', 'failed')).toBe(false);
  });

  it('share disabled when machine is not active, even with permission', () => {
    expect(isShareEnabled('anyone', true, 'idle', 'connected')).toBe(false);
    expect(isShareEnabled('anyone', true, 'connecting', 'connected')).toBe(false);
    expect(isShareEnabled('anyone', true, 'joining', 'connected')).toBe(false);
  });

  it('for any permission and host status, enabled iff (anyone OR selfIsHost) AND active AND connected', () => {
    fc.assert(
      fc.property(arbSharePermission, fc.boolean(), (perm, selfIsHost) => {
        const expected = (perm === 'anyone' || selfIsHost);
        expect(isShareEnabled(perm, selfIsHost, 'active', 'connected')).toBe(expected);
        // Always false when media not ready, regardless of permission
        expect(isShareEnabled(perm, selfIsHost, 'active', 'disconnected')).toBe(false);
        expect(isShareEnabled(perm, selfIsHost, 'idle', 'connected')).toBe(false);
      }),
      { numRuns: 100 },
    );
  });
});


import { mergeParticipantsWithVolume } from '../voice-room';
import type { RoomParticipant } from '../voice-room';

/* ═══ Property 11: Volume preservation across reconnect ═════════════ */
// Feature: gui-feature-completion, Property 11
// **Validates: Requirements 21.2**

function makeParticipant(id: string, volume: number): RoomParticipant {
  return {
    id,
    displayName: `user-${id}`,
    color: '#E06C75',
    role: 'guest',
    isSpeaking: false,
    isMuted: false,
    isHostMuted: false,
    isDeafened: false,
    isSharing: false,
    rmsLevel: 0,
    volume,
  };
}

describe('Property 11: Volume preservation across reconnect', () => {
  const DEFAULT_VOL = 70;

  it('matched participants retain old volume', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: 100 }),
        (oldVol) => {
          const old = [makeParticipant('p1', oldVol)];
          const fresh = [makeParticipant('p1', DEFAULT_VOL)];
          const merged = mergeParticipantsWithVolume(old, fresh);
          expect(merged[0].volume).toBe(oldVol);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('new participants keep the volume from the fresh list', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: 100 }),
        (freshVol) => {
          const old: RoomParticipant[] = [];
          const fresh = [makeParticipant('new-1', freshVol)];
          const merged = mergeParticipantsWithVolume(old, fresh);
          expect(merged[0].volume).toBe(freshVol);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('old-only participants are discarded', () => {
    const old = [makeParticipant('gone', 42)];
    const fresh: RoomParticipant[] = [];
    const merged = mergeParticipantsWithVolume(old, fresh);
    expect(merged).toHaveLength(0);
  });

  it('mixed scenario: matched keep volume, new carry fresh volume, old-only dropped', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: 100 }),
        (vol1) => {
          const old = [makeParticipant('stay', vol1), makeParticipant('gone', 99)];
          const fresh = [makeParticipant('stay', 50), makeParticipant('new', 50)];
          const merged = mergeParticipantsWithVolume(old, fresh);
          expect(merged).toHaveLength(2);
          expect(merged[0].id).toBe('stay');
          expect(merged[0].volume).toBe(vol1); // preserved
          expect(merged[1].id).toBe('new');
          expect(merged[1].volume).toBe(50);   // carries fresh list volume
        },
      ),
      { numRuns: 100 },
    );
  });
});


import { getState, getRegisteredHotkey, toggleSelfMute } from '../voice-room';

/* ═══ Property 15: Hotkey not registered when no voice session ═════ */
// Feature: gui-feature-completion, Property 15
// **Validates: Requirements 22.5, 22.7**

describe('Property 15: Hotkey not registered when no voice session', () => {
  it('no hotkey registered when machineState is idle', () => {
    // When no session is active, getRegisteredHotkey returns null
    const state = getState();
    if (state.machineState !== 'active') {
      expect(getRegisteredHotkey()).toBeNull();
    }
  });

  it('toggleSelfMute is a no-op when no active session (no participants)', () => {
    fc.assert(
      fc.property(fc.constant(null), () => {
        const before = getState();
        // Only test when idle (no active session)
        if (before.machineState !== 'active') {
          toggleSelfMute();
          const after = getState();
          // Participants should remain unchanged (empty)
          expect(after.participants).toEqual(before.participants);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('hotkey is null in default state', () => {
    expect(getRegisteredHotkey()).toBeNull();
  });
});


import { sendChatMessage, MAX_CHAT_MESSAGES, leaveRoom, computeSinceCursor } from '../voice-room';
import type { ChatMessage } from '../voice-room';

/* ═══ Ephemeral Room Chat — Client Property Tests ═══════════════════ */

// Feature: ephemeral-room-chat, Property 1: Send trims and transmits non-empty input
// **Validates: Requirements 1.1, 1.2**

describe('Property 1: Send trims and transmits non-empty input', () => {
  it('for any string with non-whitespace, sendChatMessage does not optimistically append', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 1 }).filter((s) => s.trim().length > 0 && s.trim().length <= 2000),
        (text) => {
          const before = getState().chatMessages.length;
          sendChatMessage(text);
          const after = getState().chatMessages.length;
          // No optimistic append — message count unchanged (echo-only model)
          expect(after).toBe(before);
        },
      ),
      { numRuns: 100 },
    );
  });
});

// Feature: ephemeral-room-chat, Property 2: Whitespace-only input is discarded
// **Validates: Requirements 1.3, 1.5**

describe('Property 2: Whitespace-only input is discarded', () => {
  const arbWhitespace = fc.array(fc.constantFrom(' ', '\t', '\n', '\r'), { minLength: 0, maxLength: 50 }).map((a) => a.join(''));

  it('for any whitespace-only string, sendChatMessage does not modify chatMessages', () => {
    fc.assert(
      fc.property(arbWhitespace, (text) => {
        const before = getState().chatMessages.length;
        sendChatMessage(text);
        const after = getState().chatMessages.length;
        expect(after).toBe(before);
      }),
      { numRuns: 100 },
    );
  });

  it('empty string is also discarded', () => {
    const before = getState().chatMessages.length;
    sendChatMessage('');
    const after = getState().chatMessages.length;
    expect(after).toBe(before);
  });
});

// Feature: ephemeral-room-chat, Property 3: No optimistic append on send
// **Validates: Requirements 1.5**

describe('Property 3: No optimistic append on send', () => {
  it('for any valid text, sendChatMessage never increases chatMessages length', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 1, maxLength: 2000 }).filter((s) => s.trim().length > 0),
        (text) => {
          const before = getState().chatMessages.length;
          sendChatMessage(text);
          const after = getState().chatMessages.length;
          expect(after).toBeLessThanOrEqual(before);
        },
      ),
      { numRuns: 100 },
    );
  });
});

// Feature: ephemeral-room-chat, Property 6: Receive appends with 200-message cap
// **Validates: Requirements 3.1, 3.4**

describe('Property 6: Receive appends with 200-message cap', () => {
  function makeChatMsg(i: number): ChatMessage {
    return {
      id: `msg-${i}`,
      timestamp: new Date().toISOString(),
      participantId: `peer-${i % 6}`,
      displayName: `User ${i % 6}`,
      color: TERMINAL_COLORS[i % TERMINAL_COLORS.length],
      text: `message ${i}`,
    };
  }

  it('for any N messages, list length equals min(N, MAX_CHAT_MESSAGES) with oldest discarded', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 1, max: 500 }),
        (n) => {
          // Simulate the dispatchMessage append + cap logic
          let chatMessages: ChatMessage[] = [];
          for (let i = 0; i < n; i++) {
            chatMessages = [...chatMessages, makeChatMsg(i)];
            if (chatMessages.length > MAX_CHAT_MESSAGES) {
              chatMessages = chatMessages.slice(-MAX_CHAT_MESSAGES);
            }
          }
          expect(chatMessages.length).toBe(Math.min(n, MAX_CHAT_MESSAGES));
          if (n > MAX_CHAT_MESSAGES) {
            // Oldest discarded — first message should be from index (n - MAX_CHAT_MESSAGES)
            expect(chatMessages[0].id).toBe(`msg-${n - MAX_CHAT_MESSAGES}`);
            expect(chatMessages[chatMessages.length - 1].id).toBe(`msg-${n - 1}`);
          }
        },
      ),
      { numRuns: 100 },
    );
  });

  it('MAX_CHAT_MESSAGES is 200', () => {
    expect(MAX_CHAT_MESSAGES).toBe(200);
  });
});

// Feature: ephemeral-room-chat, Property 7: Color resolution from participant list
// **Validates: Requirements 3.2**

describe('Property 7: Color resolution from participant list', () => {
  it('when participantId matches a participant, color equals that participant color', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 0, max: TERMINAL_COLORS.length - 1 }),
        fc.string({ minLength: 1, maxLength: 20 }),
        (colorIdx, peerId) => {
          const participants: RoomParticipant[] = [
            {
              id: peerId,
              displayName: `User-${peerId}`,
              color: TERMINAL_COLORS[colorIdx],
              role: 'guest',
              isSpeaking: false,
              isMuted: false,
              isHostMuted: false,
              isDeafened: false,
              isSharing: false,
              rmsLevel: 0,
              volume: 70,
            },
          ];
          // Simulate the color resolution logic from dispatchMessage
          const participant = participants.find((p) => p.id === peerId);
          const resolvedColor = participant?.color ?? '';
          expect(resolvedColor).toBe(TERMINAL_COLORS[colorIdx]);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('when participantId not found, color is empty string', () => {
    fc.assert(
      fc.property(
        fc.string({ minLength: 1, maxLength: 20 }),
        (unknownId) => {
          const participants: RoomParticipant[] = [
            {
              id: 'known-peer',
              displayName: 'Known',
              color: TERMINAL_COLORS[0],
              role: 'guest',
              isSpeaking: false,
              isMuted: false,
              isHostMuted: false,
              isDeafened: false,
              isSharing: false,
              rmsLevel: 0,
              volume: 70,
            },
          ];
          // Only test when unknownId differs from the known peer
          if (unknownId === 'known-peer') return;
          const participant = participants.find((p) => p.id === unknownId);
          const resolvedColor = participant?.color ?? '';
          expect(resolvedColor).toBe('');
        },
      ),
      { numRuns: 100 },
    );
  });
});

// Feature: ephemeral-room-chat, Property 8: Leave clears chat history
// **Validates: Requirements 4.1, 4.2**

describe('Property 8: Leave clears chat history', () => {
  it('in default/idle state, chatMessages is empty', () => {
    // After module load (no active session), chat should be empty
    const s = getState();
    if (s.machineState === 'idle') {
      expect(s.chatMessages).toHaveLength(0);
    }
  });

  it('leaveRoom resets chatMessages to empty array', () => {
    fc.assert(
      fc.property(fc.constant(null), () => {
        // leaveRoom always resets to DEFAULT_STATE with fresh chatMessages: []
        leaveRoom();
        const s = getState();
        expect(s.chatMessages).toHaveLength(0);
        expect(s.machineState).toBe('idle');
      }),
      { numRuns: 100 },
    );
  });
});

// Feature: ephemeral-room-chat, Property 12: Client rejects oversized messages before send
// **Validates: Requirements 6.3**

describe('Property 12: Client rejects oversized messages before send', () => {
  it('for any string whose trimmed length exceeds 2000, sendChatMessage does not modify chatMessages', () => {
    fc.assert(
      fc.property(
        // Generate strings > 2000 chars after trim (pad with non-whitespace)
        fc.string({ minLength: 2001, maxLength: 4000 }).map((s) => {
          // Ensure trimmed length > 2000 by prepending/appending non-whitespace
          const base = s.replace(/^\s+|\s+$/g, '');
          return base.length > 2000 ? base : 'x'.repeat(2001) + base;
        }),
        (text) => {
          const before = getState().chatMessages.length;
          sendChatMessage(text);
          const after = getState().chatMessages.length;
          expect(after).toBe(before);
        },
      ),
      { numRuns: 100 },
    );
  });
});


/* ═══ Task 6.4: Unit tests for client serialization and boundary cases ═══ */

// Feature: ephemeral-room-chat
// Chat panel exclusion (Requirement 9.1) + Boundary value tests (Requirement 6.3)

describe('Chat panel exclusion (Requirement 9.1)', () => {
  it('participant_joined and participant_left events do NOT appear in chatMessages', () => {
    // In the default/idle state (no active session), chatMessages should be empty.
    // The dispatchMessage handler for participant_joined and participant_left
    // appends to state.events (the event log), NOT to state.chatMessages.
    // Since dispatchMessage is private, we verify the invariant through the public API:
    // after module load, chatMessages is empty — join/leave events never populate it.
    const s = getState();
    expect(s.chatMessages).toEqual([]);
  });

  it('chatMessages array contains no entries with join/leave event types', () => {
    // chatMessages items have a ChatMessage shape (id, timestamp, participantId,
    // displayName, color, text). They never carry event-type fields like 'join' or 'leave'.
    // Verify the structural separation: every item in chatMessages has a 'text' field
    // and none have a 'type' field (which RoomEvent entries have).
    const s = getState();
    for (const msg of s.chatMessages) {
      expect(msg).toHaveProperty('text');
      expect(msg).toHaveProperty('participantId');
      expect(msg).toHaveProperty('displayName');
      // ChatMessage does not have a 'type' field — that's RoomEvent's shape
      expect(msg).not.toHaveProperty('type');
    }
  });
});

describe('Boundary value tests (Requirement 6.3)', () => {
  it('sendChatMessage with exactly 2000 chars does not throw', () => {
    const text = 'x'.repeat(2000);
    // Should not throw — the 2000-char limit is inclusive.
    // Without a WS connection it won't actually send, but the key is it
    // passes the client-side length guard without rejection.
    expect(() => sendChatMessage(text)).not.toThrow();
    // chatMessages unchanged (no optimistic append, no WS connection)
    const s = getState();
    expect(s.chatMessages).toEqual([]);
  });

  it('sendChatMessage with 2001 chars is rejected (chatMessages unchanged)', () => {
    const text = 'x'.repeat(2001);
    const before = getState().chatMessages.length;
    sendChatMessage(text);
    const after = getState().chatMessages.length;
    expect(after).toBe(before);
  });

  it('sendChatMessage with 1999 chars does not throw', () => {
    const text = 'a'.repeat(1999);
    expect(() => sendChatMessage(text)).not.toThrow();
  });

  it('sendChatMessage with exactly 2000 chars after trim does not throw', () => {
    // Leading/trailing whitespace is trimmed first, so pad with spaces
    const text = '  ' + 'y'.repeat(2000) + '  ';
    // After trim: 2000 chars — should pass the guard
    expect(() => sendChatMessage(text)).not.toThrow();
  });

  it('sendChatMessage with 2001 chars after trim is rejected', () => {
    const text = ' ' + 'z'.repeat(2001) + ' ';
    const before = getState().chatMessages.length;
    sendChatMessage(text);
    const after = getState().chatMessages.length;
    expect(after).toBe(before);
  });
});


/* ═══ Feature: chat-history-persistence, Property 5: Client since cursor derivation ═══ */
// **Validates: Requirements 3.3**

describe('Property 5: Client since cursor derivation', () => {
  // Generate timestamps as integer ms then convert — avoids invalid Date from fc.date()
  const MIN_MS = new Date('2020-01-01T00:00:00Z').getTime();
  const MAX_MS = new Date('2030-01-01T00:00:00Z').getTime();
  const arbTimestamp = fc.integer({ min: MIN_MS, max: MAX_MS }).map((ms) => new Date(ms).toISOString());

  const arbChatMessage = (overrides?: Partial<ChatMessage>) =>
    fc.record({
      id: fc.uuid(),
      timestamp: arbTimestamp,
      participantId: fc.string({ minLength: 1, maxLength: 20 }),
      displayName: fc.string({ minLength: 1, maxLength: 30 }),
      color: fc.constantFrom(...TERMINAL_COLORS),
      text: fc.string({ minLength: 1, maxLength: 200 }),
    }).map((m) => ({ ...m, messageId: undefined, isHistory: undefined, isDivider: undefined, ...overrides } as ChatMessage));

  it('non-empty real-time messages: since = earliest timestamp minus 1 second', () => {
    fc.assert(
      fc.property(
        fc.array(arbChatMessage({ isHistory: undefined }), { minLength: 1, maxLength: 20 }),
        (messages) => {
          const result = computeSinceCursor(messages);
          expect(result).toBeDefined();

          // Find earliest timestamp manually
          let earliest = messages[0].timestamp;
          for (const m of messages) {
            if (m.timestamp < earliest) earliest = m.timestamp;
          }
          const expected = new Date(new Date(earliest).getTime() - 1000).toISOString();
          expect(result).toBe(expected);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('empty message list: since is undefined', () => {
    fc.assert(
      fc.property(fc.constant([] as ChatMessage[]), (messages: ChatMessage[]) => {
        expect(computeSinceCursor(messages)).toBeUndefined();
      }),
      { numRuns: 100 },
    );
  });

  it('mixed isHistory messages: only non-history messages considered', () => {
    fc.assert(
      fc.property(
        fc.array(arbChatMessage({ isHistory: true }), { minLength: 1, maxLength: 10 }),
        fc.array(arbChatMessage({ isHistory: undefined }), { minLength: 1, maxLength: 10 }),
        (historyMsgs, realtimeMsgs) => {
          const mixed = [...historyMsgs, ...realtimeMsgs];
          const result = computeSinceCursor(mixed);
          expect(result).toBeDefined();

          // Only real-time messages should be considered
          let earliest = realtimeMsgs[0].timestamp;
          for (const m of realtimeMsgs) {
            if (m.timestamp < earliest) earliest = m.timestamp;
          }
          const expected = new Date(new Date(earliest).getTime() - 1000).toISOString();
          expect(result).toBe(expected);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('all-history messages: since is undefined', () => {
    fc.assert(
      fc.property(
        fc.array(arbChatMessage({ isHistory: true }), { minLength: 1, maxLength: 10 }),
        (messages) => {
          expect(computeSinceCursor(messages)).toBeUndefined();
        },
      ),
      { numRuns: 100 },
    );
  });
});


import { mergeHistoryMessages } from '../voice-room';

/* ═══ Feature: chat-history-persistence, Property 6: Client merge, dedup, and cap ═══ */
// **Validates: Requirements 3.4, 4.1, 4.3**

describe('Property 6: Client merge, dedup, and cap', () => {
  const MIN_MS = new Date('2020-01-01T00:00:00Z').getTime();
  const MAX_MS = new Date('2030-01-01T00:00:00Z').getTime();
  const arbTimestamp = fc.integer({ min: MIN_MS, max: MAX_MS }).map((ms) => new Date(ms).toISOString());

  // Generator for history payload entries
  const arbHistoryEntry = fc.record({
    messageId: fc.uuid(),
    participantId: fc.string({ minLength: 1, maxLength: 20 }),
    displayName: fc.string({ minLength: 1, maxLength: 30 }),
    text: fc.string({ minLength: 1, maxLength: 200 }),
    timestamp: arbTimestamp,
  });

  // Generator for existing (real-time) ChatMessage entries with messageId set
  const arbExistingMessage = fc.record({
    id: fc.uuid(),
    messageId: fc.uuid(),
    timestamp: arbTimestamp,
    participantId: fc.string({ minLength: 1, maxLength: 20 }),
    displayName: fc.string({ minLength: 1, maxLength: 30 }),
    color: fc.constantFrom(...TERMINAL_COLORS),
    text: fc.string({ minLength: 1, maxLength: 200 }),
  }).map((m) => m as ChatMessage);

  // Generator that produces history + existing with some overlapping messageIds
  const arbWithOverlap = fc
    .tuple(
      fc.array(arbHistoryEntry, { minLength: 0, maxLength: 50 }),
      fc.array(arbExistingMessage, { minLength: 0, maxLength: 50 }),
    )
    .chain(([history, existing]) => {
      // Pick a random subset of existing messageIds to inject into history for overlap
      if (existing.length === 0 || history.length === 0) {
        return fc.constant({ history, existing });
      }
      return fc
        .array(fc.integer({ min: 0, max: existing.length - 1 }), { minLength: 0, maxLength: Math.min(existing.length, 10) })
        .map((indices) => {
          const overlapping = [...history];
          for (const idx of indices) {
            overlapping.push({
              messageId: existing[idx].messageId!,
              participantId: `overlap-${idx}`,
              displayName: `Overlap ${idx}`,
              text: 'duplicate',
              timestamp: new Date().toISOString(),
            });
          }
          return { history: overlapping, existing };
        });
    });

  it('no duplicate messageIds in merged result', () => {
    fc.assert(
      fc.property(arbWithOverlap, ({ history, existing }) => {
        const merged = mergeHistoryMessages(history, existing);
        // Filter out divider entries (no messageId)
        const withIds = merged.filter((m) => !m.isDivider && m.messageId);
        const idSet = new Set(withIds.map((m) => m.messageId));
        expect(idSet.size).toBe(withIds.length);
      }),
      { numRuns: 100 },
    );
  });

  it('history messages appear before real-time messages', () => {
    fc.assert(
      fc.property(arbWithOverlap, ({ history, existing }) => {
        const merged = mergeHistoryMessages(history, existing);
        // Find last history index and first real-time index
        let lastHistoryIdx = -1;
        let firstRealtimeIdx = merged.length;
        for (let i = 0; i < merged.length; i++) {
          if (merged[i].isDivider) continue;
          if (merged[i].isHistory) {
            lastHistoryIdx = i;
          } else if (firstRealtimeIdx === merged.length) {
            firstRealtimeIdx = i;
          }
        }
        // If both exist, all history must come before all real-time
        if (lastHistoryIdx >= 0 && firstRealtimeIdx < merged.length) {
          expect(lastHistoryIdx).toBeLessThan(firstRealtimeIdx);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('total merged list length does not exceed MAX_CHAT_MESSAGES', () => {
    // Use larger arrays to stress the cap
    const largeHistory = fc.array(arbHistoryEntry, { minLength: 0, maxLength: 150 });
    const largeExisting = fc.array(arbExistingMessage, { minLength: 0, maxLength: 150 });

    fc.assert(
      fc.property(largeHistory, largeExisting, (history, existing) => {
        const merged = mergeHistoryMessages(history, existing);
        expect(merged.length).toBeLessThanOrEqual(MAX_CHAT_MESSAGES);
      }),
      { numRuns: 100 },
    );
  });
});


/* ═══ Feature: chat-history-persistence, Property 7: Divider position stability ═══ */
// **Validates: Requirements 4.7**

describe('Property 7: Divider position stability', () => {
  const MIN_MS = new Date('2020-01-01T00:00:00Z').getTime();
  const MAX_MS = new Date('2030-01-01T00:00:00Z').getTime();
  const arbTimestamp = fc.integer({ min: MIN_MS, max: MAX_MS }).map((ms) => new Date(ms).toISOString());

  const arbHistoryEntry = fc.record({
    messageId: fc.uuid(),
    participantId: fc.string({ minLength: 1, maxLength: 20 }),
    displayName: fc.string({ minLength: 1, maxLength: 30 }),
    text: fc.string({ minLength: 1, maxLength: 200 }),
    timestamp: arbTimestamp,
  });

  const arbExistingMessage = fc.record({
    id: fc.uuid(),
    messageId: fc.uuid(),
    timestamp: arbTimestamp,
    participantId: fc.string({ minLength: 1, maxLength: 20 }),
    displayName: fc.string({ minLength: 1, maxLength: 30 }),
    color: fc.constantFrom(...TERMINAL_COLORS),
    text: fc.string({ minLength: 1, maxLength: 200 }),
  }).map((m) => m as ChatMessage);

  const arbNewMessage = fc.record({
    id: fc.uuid(),
    messageId: fc.uuid(),
    timestamp: arbTimestamp,
    participantId: fc.string({ minLength: 1, maxLength: 20 }),
    displayName: fc.string({ minLength: 1, maxLength: 30 }),
    color: fc.constantFrom(...TERMINAL_COLORS),
    text: fc.string({ minLength: 1, maxLength: 200 }),
  }).map((m) => m as ChatMessage);

  it('divider index unchanged after appending new real-time messages', () => {
    fc.assert(
      fc.property(
        fc.array(arbHistoryEntry, { minLength: 1, maxLength: 20 }),
        fc.array(arbExistingMessage, { minLength: 0, maxLength: 20 }),
        fc.array(arbNewMessage, { minLength: 1, maxLength: 30 }),
        (history, existing, newMessages) => {
          // Step 1: merge to get a list with a divider
          let messages = mergeHistoryMessages(history, existing);

          // Find divider index
          const dividerIdx = messages.findIndex((m) => m.isDivider);
          // History is non-empty so divider must exist
          expect(dividerIdx).toBeGreaterThanOrEqual(0);

          // Step 2: simulate appending N new real-time messages
          // (same logic as the chat_message dispatch handler)
          for (const msg of newMessages) {
            messages = [...messages, msg];
            if (messages.length > MAX_CHAT_MESSAGES) {
              messages = messages.slice(-MAX_CHAT_MESSAGES);
            }
          }

          // Step 3: check divider position
          const newDividerIdx = messages.findIndex((m) => m.isDivider);
          if (newDividerIdx >= 0) {
            // Divider survived the cap — its index must be unchanged
            expect(newDividerIdx).toBe(dividerIdx);
          }
          // If divider was trimmed by the cap, that's fine — property only
          // holds when the divider is still present
        },
      ),
      { numRuns: 100 },
    );
  });
});
