/**
 * Property-based tests for the screen share bridge composite key refactor.
 *
 * Uses fast-check to verify correctness properties of the senders Map
 * keyed by composite `participantId::windowLabel` keys.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import fc from 'fast-check';

/* ─── Mocks ─────────────────────────────────────────────────────── */

vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn(),
  listen: vi.fn().mockResolvedValue(() => {}),
}));

class MockRTCPeerConnection {
  onicecandidate: ((e: unknown) => void) | null = null;
  ontrack: ((e: unknown) => void) | null = null;
  connectionState = 'new';
  addTrack = vi.fn();
  createOffer = vi.fn().mockResolvedValue({ sdp: 'mock-offer', type: 'offer' });
  createAnswer = vi.fn().mockResolvedValue({ sdp: 'mock-answer', type: 'answer' });
  setLocalDescription = vi.fn().mockResolvedValue(undefined);
  setRemoteDescription = vi.fn().mockResolvedValue(undefined);
  addIceCandidate = vi.fn().mockResolvedValue(undefined);
  close = vi.fn();
}
globalThis.RTCPeerConnection = MockRTCPeerConnection as unknown as typeof RTCPeerConnection;

class MockMediaStream {
  private tracks: unknown[] = [];
  getTracks() { return this.tracks; }
  addTrack(t: unknown) { this.tracks.push(t); }
}
globalThis.MediaStream = MockMediaStream as unknown as typeof MediaStream;

import {
  compositeKey,
  stopSending,
  stopSendingForWindow,
  stopAllSending,
  _getSendersForTest,
} from '../screen-share-viewer';

/* ─── Arbitraries ───────────────────────────────────────────────── */

const participantIdArb = fc.stringMatching(/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/);
const windowLabelArb = fc.constantFrom('watch-all', 'screen-share-a', 'screen-share-b', 'screen-share-c');

/** Generate a non-empty list of unique (participantId, windowLabel) pairs. */
const pairsArb = fc
  .array(fc.tuple(participantIdArb, windowLabelArb), { minLength: 1, maxLength: 8 })
  .map((pairs) => {
    const seen = new Set<string>();
    return pairs.filter(([pid, wl]) => {
      const k = compositeKey(pid, wl);
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
  })
  .filter((pairs) => pairs.length > 0);

/* ─── Helpers ───────────────────────────────────────────────────── */

/** Populate the senders Map directly with mock entries (avoids async startSending). */
function populateSenders(pairs: [string, string][]): void {
  const senders = _getSendersForTest();
  for (const [pid, wl] of pairs) {
    const key = compositeKey(pid, wl);
    senders.set(key, {
      pc: new RTCPeerConnection() as unknown as RTCPeerConnection,
      cleanups: [],
      offerSdp: null,
    });
  }
}

/* ═══ Property Tests ════════════════════════════════════════════════ */

beforeEach(() => {
  stopAllSending();
});

describe('Property 3: Window-scoped sender cleanup', () => {
  // Feature: watch-all-streams, Property 3: Window-scoped sender cleanup
  // **Validates: Requirements 1.3, 7.1, 7.3, 7.4, 14.2**

  it('stopSendingForWindow removes exactly entries matching the target label', () => {
    fc.assert(
      fc.property(pairsArb, windowLabelArb, (pairs, targetLabel) => {
        stopAllSending();
        populateSenders(pairs);

        const senders = _getSendersForTest();
        const beforeKeys = new Set(senders.keys());

        stopSendingForWindow(targetLabel);

        // No remaining entries should have the target label
        for (const key of senders.keys()) {
          expect(key.endsWith(`::${targetLabel}`)).toBe(false);
        }

        // All entries with other labels should still be present
        for (const key of beforeKeys) {
          if (!key.endsWith(`::${targetLabel}`)) {
            expect(senders.has(key)).toBe(true);
          }
        }
      }),
      { numRuns: 100 },
    );
  });
});


describe('Property 4: Composite key allows concurrent senders per participant', () => {
  // Feature: watch-all-streams, Property 4: Composite key allows concurrent senders per participant
  // **Validates: Requirements 7.2, 12.1**

  it('two distinct window labels for the same participant create two entries', () => {
    fc.assert(
      fc.property(
        participantIdArb,
        fc.tuple(windowLabelArb, windowLabelArb).filter(([a, b]) => a !== b),
        (pid, [wl1, wl2]) => {
          stopAllSending();
          populateSenders([[pid, wl1], [pid, wl2]]);

          const senders = _getSendersForTest();
          const key1 = compositeKey(pid, wl1);
          const key2 = compositeKey(pid, wl2);

          expect(senders.size).toBe(2);
          expect(senders.has(key1)).toBe(true);
          expect(senders.has(key2)).toBe(true);
        },
      ),
      { numRuns: 100 },
    );
  });
});

describe('Property 5: Sender operations scoped to composite key', () => {
  // Feature: watch-all-streams, Property 5: Sender operations are scoped to their composite key
  // **Validates: Requirements 12.2, 12.4**

  it('stopSending removes only the chosen pair, all others remain', () => {
    fc.assert(
      fc.property(
        pairsArb.filter((p) => p.length >= 2),
        fc.nat(),
        (pairs, rawIdx) => {
          stopAllSending();
          populateSenders(pairs);

          const chosenIdx = rawIdx % pairs.length;
          const [chosenPid, chosenWl] = pairs[chosenIdx];
          const chosenKey = compositeKey(chosenPid, chosenWl);

          stopSending(chosenPid, chosenWl);

          const senders = _getSendersForTest();

          // Chosen pair must be removed
          expect(senders.has(chosenKey)).toBe(false);

          // All other pairs must remain
          for (let i = 0; i < pairs.length; i++) {
            if (i === chosenIdx) continue;
            const [pid, wl] = pairs[i];
            expect(senders.has(compositeKey(pid, wl))).toBe(true);
          }
        },
      ),
      { numRuns: 100 },
    );
  });
});

describe('Property 6: stopAllSending empties the senders map', () => {
  // Feature: watch-all-streams, Property 6: stopAllSending empties the senders map
  // **Validates: Requirements 12.3**

  it('senders map is empty after stopAllSending', () => {
    fc.assert(
      fc.property(pairsArb, (pairs) => {
        stopAllSending();
        populateSenders(pairs);

        expect(_getSendersForTest().size).toBeGreaterThan(0);

        stopAllSending();

        expect(_getSendersForTest().size).toBe(0);
      }),
      { numRuns: 100 },
    );
  });
});
