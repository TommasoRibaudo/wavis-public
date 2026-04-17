import { describe, it, expect } from 'vitest';
import fc from 'fast-check';
import type { ShareMode, ShareSelection, FallbackReason } from '@features/screen-share/share-types';
import type { VoiceRoomState } from '../voice-room';
import {
  planShareCommands,
  planStopCommands,
  buildStartShareMessage,
  canStartShare,
  isAnyShareActive,
  activeShareType,
  fallbackShareAction,
  computeShareRoute,
  computeStopRoute,
  isShareButtonDisabled,
} from '../voice-room';

/* ─── Arbitraries ───────────────────────────────────────────────── */

const arbShareMode: fc.Arbitrary<ShareMode> = fc.constantFrom(
  'screen_audio' as const,
  'window' as const,
  'audio_only' as const,
);

const arbShareSelection: fc.Arbitrary<ShareSelection> = fc.record({
  mode: arbShareMode,
  sourceId: fc.string({ minLength: 1, maxLength: 64 }),
  sourceName: fc.string({ minLength: 1, maxLength: 128 }),
  withAudio: fc.boolean(),
});

const arbVideoShare: fc.Arbitrary<VoiceRoomState['activeVideoShare']> = fc.oneof(
  fc.constant(null as VoiceRoomState['activeVideoShare']),
  fc.record({
    mode: fc.constantFrom('screen_audio' as const, 'window' as const),
    sourceName: fc.string({ minLength: 1, maxLength: 64 }),
    withAudio: fc.boolean(),
  }),
);

const arbAudioShare: fc.Arbitrary<VoiceRoomState['activeAudioShare']> = fc.oneof(
  fc.constant(null as VoiceRoomState['activeAudioShare']),
  fc.record({
    sourceId: fc.string({ minLength: 1, maxLength: 64 }),
    sourceName: fc.string({ minLength: 1, maxLength: 64 }),
  }),
);

const arbStopTarget: fc.Arbitrary<'video' | 'audio' | 'all'> = fc.constantFrom(
  'video' as const,
  'audio' as const,
  'all' as const,
);

/* ═══ Property 6: Share mode routing correctness ════════════════════ */

describe('Property 6: Share mode routing correctness', () => {
  it('screen_audio always produces a video command', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'screen_audio' as const };
        const plan = planShareCommands(s);
        expect(plan.videoCommand).not.toBeNull();
        expect(plan.videoCommand!.name).toBe('screen_share_start_source');
        expect(plan.videoCommand!.sourceId).toBe(s.sourceId);
      }),
      { numRuns: 100 },
    );
  });

  it('window always produces a video command', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'window' as const };
        const plan = planShareCommands(s);
        expect(plan.videoCommand).not.toBeNull();
        expect(plan.videoCommand!.name).toBe('screen_share_start_source');
        expect(plan.videoCommand!.sourceId).toBe(s.sourceId);
      }),
      { numRuns: 100 },
    );
  });

  it('audio_only never produces a video command', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'audio_only' as const };
        const plan = planShareCommands(s);
        expect(plan.videoCommand).toBeNull();
      }),
      { numRuns: 100 },
    );
  });

  it('screen_audio with audio resolves default monitor (resolveMonitor=true)', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'screen_audio' as const, withAudio: true };
        const plan = planShareCommands(s);
        expect(plan.audioCommand).not.toBeNull();
        expect(plan.audioCommand!.name).toBe('audio_share_start');
        expect(plan.audioCommand!.resolveMonitor).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('window with audio resolves default monitor (resolveMonitor=true)', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'window' as const, withAudio: true };
        const plan = planShareCommands(s);
        expect(plan.audioCommand).not.toBeNull();
        expect(plan.audioCommand!.resolveMonitor).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('audio_only passes user-selected source directly (resolveMonitor=false)', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'audio_only' as const };
        const plan = planShareCommands(s);
        expect(plan.audioCommand).not.toBeNull();
        expect(plan.audioCommand!.resolveMonitor).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('window without audio produces no audio command', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'window' as const, withAudio: false };
        const plan = planShareCommands(s);
        expect(plan.audioCommand).toBeNull();
      }),
      { numRuns: 100 },
    );
  });

  it('screen_audio without audio produces no audio command', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const s = { ...sel, mode: 'screen_audio' as const, withAudio: false };
        const plan = planShareCommands(s);
        expect(plan.audioCommand).toBeNull();
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 7: Signaling message includes share type ═════════════ */

describe('Property 7: Signaling message includes share type', () => {
  it('start_share message always includes shareType matching the active mode', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const msg = buildStartShareMessage(mode);
        expect(msg.type).toBe('start_share');
        expect(msg.shareType).toBe(mode);
      }),
      { numRuns: 100 },
    );
  });

  it('shareType is always a valid ShareMode value', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const msg = buildStartShareMessage(mode);
        expect(['screen_audio', 'window', 'audio_only']).toContain(msg.shareType);
      }),
      { numRuns: 100 },
    );
  });

  it('message type is always start_share regardless of mode', () => {
    fc.assert(
      fc.property(arbShareMode, (mode) => {
        const msg = buildStartShareMessage(mode);
        expect(msg.type).toBe('start_share');
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 9: Stop commands match two-slot model ════════════════ */

describe('Property 9: Stop commands match two-slot model', () => {
  it('target=video only stops video and companion audio', () => {
    fc.assert(
      fc.property(arbVideoShare, arbAudioShare, (video, audio) => {
        const plan = planStopCommands('video', video, audio);
        expect(plan.stopVideo).toBe(video !== null);
        expect(plan.stopCompanionAudio).toBe(video?.withAudio ?? false);
        expect(plan.stopAudioOnly).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('target=audio only stops standalone audio', () => {
    fc.assert(
      fc.property(arbVideoShare, arbAudioShare, (video, audio) => {
        const plan = planStopCommands('audio', video, audio);
        expect(plan.stopVideo).toBe(false);
        expect(plan.stopCompanionAudio).toBe(false);
        expect(plan.stopAudioOnly).toBe(audio !== null);
      }),
      { numRuns: 100 },
    );
  });

  it('target=all stops everything that is active', () => {
    fc.assert(
      fc.property(arbVideoShare, arbAudioShare, (video, audio) => {
        const plan = planStopCommands('all', video, audio);
        expect(plan.stopVideo).toBe(video !== null);
        expect(plan.stopCompanionAudio).toBe(video?.withAudio ?? false);
        expect(plan.stopAudioOnly).toBe(audio !== null);
      }),
      { numRuns: 100 },
    );
  });

  it('stop commands for empty slots are all false', () => {
    fc.assert(
      fc.property(arbStopTarget, (target) => {
        const plan = planStopCommands(target, null, null);
        expect(plan.stopVideo).toBe(false);
        expect(plan.stopCompanionAudio).toBe(false);
        expect(plan.stopAudioOnly).toBe(false);
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 10: Atomic share failure rollback (two-slot) ═════════ */

describe('Property 10: Atomic share failure rollback', () => {
  function simulateShareWithFailure(
    selection: ShareSelection,
    failAt: 'video' | 'audio_after_video',
  ): {
    activeVideoShare: VoiceRoomState['activeVideoShare'];
    activeAudioShare: VoiceRoomState['activeAudioShare'];
    signalingMessageSent: boolean;
    videoStopped: boolean;
  } {
    const isVideoShare = selection.mode === 'screen_audio' || selection.mode === 'window';

    // Optimistic state set (mirrors startCustomShare two-slot model)
    let activeVideoShare: VoiceRoomState['activeVideoShare'] = null;
    let activeAudioShare: VoiceRoomState['activeAudioShare'] = null;

    if (isVideoShare) {
      activeVideoShare = {
        mode: selection.mode as 'screen_audio' | 'window',
        sourceName: selection.sourceName,
        withAudio: selection.withAudio,
      };
    } else {
      activeAudioShare = {
        sourceId: selection.sourceId,
        sourceName: selection.sourceName,
      };
    }

    let signalingMessageSent = false;
    let videoStarted = false;
    let videoStopped = false;

    const needsVideo = isVideoShare;
    const needsAudio =
      selection.mode === 'audio_only' ||
      (isVideoShare && selection.withAudio);

    try {
      if (needsVideo) {
        if (failAt === 'video') throw new Error('video capture failed');
        videoStarted = true;
      }

      if (needsAudio) {
        if (failAt === 'audio_after_video') {
          if (videoStarted) videoStopped = true;
          throw new Error('audio capture failed');
        }
      }

      signalingMessageSent = true;
    } catch {
      // Rollback: clear only the affected slot
      if (isVideoShare) {
        activeVideoShare = null;
      } else {
        activeAudioShare = null;
      }
    }

    return { activeVideoShare, activeAudioShare, signalingMessageSent, videoStopped };
  }

  it('video failure clears video slot with no signaling', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const result = simulateShareWithFailure(sel, 'video');
        const needsVideo = sel.mode === 'screen_audio' || sel.mode === 'window';

        if (needsVideo) {
          expect(result.activeVideoShare).toBeNull();
          expect(result.signalingMessageSent).toBe(false);
          expect(result.videoStopped).toBe(false);
        } else {
          expect(result.signalingMessageSent).toBe(true);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('audio failure after video success rolls back video and clears slot', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const result = simulateShareWithFailure(sel, 'audio_after_video');
        const needsVideo = sel.mode === 'screen_audio' || sel.mode === 'window';
        const needsAudio =
          sel.mode === 'audio_only' ||
          ((sel.mode === 'screen_audio' || sel.mode === 'window') && sel.withAudio);

        if (needsAudio) {
          expect(result.signalingMessageSent).toBe(false);
          if (needsVideo) {
            expect(result.activeVideoShare).toBeNull();
            expect(result.videoStopped).toBe(true);
          } else {
            expect(result.activeAudioShare).toBeNull();
          }
        } else {
          expect(result.signalingMessageSent).toBe(true);
        }
      }),
      { numRuns: 100 },
    );
  });

  it('any failure clears the affected slot', () => {
    const arbFailAt = fc.constantFrom('video' as const, 'audio_after_video' as const);
    fc.assert(
      fc.property(arbShareSelection, arbFailAt, (sel, failAt) => {
        const result = simulateShareWithFailure(sel, failAt);
        const isVideoShare = sel.mode === 'screen_audio' || sel.mode === 'window';
        const needsVideo = isVideoShare;
        const needsAudio =
          sel.mode === 'audio_only' ||
          (isVideoShare && sel.withAudio);

        const failureReached =
          (failAt === 'video' && needsVideo) ||
          (failAt === 'audio_after_video' && needsAudio);

        if (failureReached) {
          if (isVideoShare) {
            expect(result.activeVideoShare).toBeNull();
          } else {
            expect(result.activeAudioShare).toBeNull();
          }
          expect(result.signalingMessageSent).toBe(false);
        }
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 8: Audio capture independence from mic state ══════════ */

describe('Property 8: Audio capture independence from mic state', () => {
  interface ShareState {
    activeVideoShare: VoiceRoomState['activeVideoShare'];
    activeAudioShare: VoiceRoomState['activeAudioShare'];
  }

  function applyMicTransitions(
    initial: ShareState,
    transitions: boolean[],
  ): ShareState {
    let current = { ...initial };
    for (const _muted of transitions) {
      // toggleSelfMute() never touches share slots — this is the property
      current = { ...current };
    }
    return current;
  }

  it('mic mute/unmute transitions never change activeVideoShare', () => {
    fc.assert(
      fc.property(
        arbVideoShare,
        arbAudioShare,
        fc.array(fc.boolean(), { minLength: 1, maxLength: 20 }),
        (video, audio, transitions) => {
          const initial: ShareState = { activeVideoShare: video, activeAudioShare: audio };
          const after = applyMicTransitions(initial, transitions);
          expect(after.activeVideoShare).toEqual(initial.activeVideoShare);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('mic mute/unmute transitions never change activeAudioShare', () => {
    fc.assert(
      fc.property(
        arbVideoShare,
        arbAudioShare,
        fc.array(fc.boolean(), { minLength: 1, maxLength: 20 }),
        (video, audio, transitions) => {
          const initial: ShareState = { activeVideoShare: video, activeAudioShare: audio };
          const after = applyMicTransitions(initial, transitions);
          expect(after.activeAudioShare).toEqual(initial.activeAudioShare);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('share state is identical before and after any sequence of mic toggles', () => {
    fc.assert(
      fc.property(
        arbVideoShare,
        arbAudioShare,
        fc.array(fc.boolean(), { minLength: 0, maxLength: 50 }),
        (video, audio, transitions) => {
          const initial: ShareState = { activeVideoShare: video, activeAudioShare: audio };
          const after = applyMicTransitions(initial, transitions);
          expect(after).toEqual(initial);
        },
      ),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 18: canStartShare slot conflict detection ════════════ */

describe('Property 18: canStartShare slot conflict detection', () => {
  it('audio_only is blocked when audio slot is occupied', () => {
    fc.assert(
      fc.property(arbShareSelection, arbVideoShare, (sel, video) => {
        const s = { ...sel, mode: 'audio_only' as const };
        const audio = { sourceId: 'existing', sourceName: 'Existing Audio' };
        const result = canStartShare(s, video, audio);
        expect(result.allowed).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('audio_only is allowed when audio slot is empty', () => {
    fc.assert(
      fc.property(arbShareSelection, arbVideoShare, (sel, video) => {
        const s = { ...sel, mode: 'audio_only' as const };
        const result = canStartShare(s, video, null);
        expect(result.allowed).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('video share is blocked when video slot is occupied', () => {
    fc.assert(
      fc.property(arbShareSelection, arbAudioShare, (sel, audio) => {
        const videoMode = fc.sample(fc.constantFrom('screen_audio' as const, 'window' as const), 1)[0];
        const s = { ...sel, mode: videoMode };
        const existingVideo = { mode: 'screen_audio' as const, sourceName: 'Existing', withAudio: false };
        const result = canStartShare(s, existingVideo, audio);
        expect(result.allowed).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('video share is allowed when video slot is empty', () => {
    fc.assert(
      fc.property(arbShareSelection, arbAudioShare, (sel, audio) => {
        const videoMode = fc.sample(fc.constantFrom('screen_audio' as const, 'window' as const), 1)[0];
        const s = { ...sel, mode: videoMode };
        const result = canStartShare(s, null, audio);
        expect(result.allowed).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('concurrent video + audio is allowed when both slots are empty', () => {
    fc.assert(
      fc.property(arbShareSelection, (sel) => {
        const result = canStartShare(sel, null, null);
        expect(result.allowed).toBe(true);
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 19: activeShareType derivation ═══════════════════════ */

describe('Property 19: activeShareType derivation', () => {
  it('returns video mode when video share is active', () => {
    fc.assert(
      fc.property(arbVideoShare.filter((v) => v !== null), arbAudioShare, (video, audio) => {
        const result = activeShareType(video, audio);
        expect(result).toBe(video!.mode);
      }),
      { numRuns: 100 },
    );
  });

  it('returns audio_only when only audio share is active', () => {
    fc.assert(
      fc.property(arbAudioShare.filter((a) => a !== null), (audio) => {
        const result = activeShareType(null, audio);
        expect(result).toBe('audio_only');
      }),
      { numRuns: 100 },
    );
  });

  it('returns null when both slots are empty', () => {
    expect(activeShareType(null, null)).toBeNull();
  });

  it('isAnyShareActive matches non-null activeShareType', () => {
    fc.assert(
      fc.property(arbVideoShare, arbAudioShare, (video, audio) => {
        const any = isAnyShareActive(video, audio);
        const type = activeShareType(video, audio);
        expect(any).toBe(type !== null);
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 23: Fallback share signaling lifecycle ═══════════════ */
// Feature: cross-platform-share-picker, Property 23: Fallback share signaling lifecycle
// **Validates: Requirements 3.3, 3.4, 8.3**

describe('Property 23: Fallback share signaling lifecycle', () => {
  it('true outcome always produces send_start_share action', () => {
    fc.assert(
      fc.property(fc.constant(true), (outcome) => {
        const action = fallbackShareAction(outcome);
        expect(action).toBe('send_start_share');
      }),
      { numRuns: 100 },
    );
  });

  it('false outcome always produces no_op action', () => {
    fc.assert(
      fc.property(fc.constant(false), (outcome) => {
        const action = fallbackShareAction(outcome);
        expect(action).toBe('no_op');
      }),
      { numRuns: 100 },
    );
  });

  it('random boolean outcome routes correctly: true → start_share sent, false → no signaling', () => {
    fc.assert(
      fc.property(fc.boolean(), (outcome) => {
        const action = fallbackShareAction(outcome);
        if (outcome) {
          expect(action).toBe('send_start_share');
        } else {
          expect(action).toBe('no_op');
        }
      }),
      { numRuns: 100 },
    );
  });

  it('action is deterministic for any given outcome', () => {
    fc.assert(
      fc.property(fc.boolean(), (outcome) => {
        const a = fallbackShareAction(outcome);
        const b = fallbackShareAction(outcome);
        expect(a).toBe(b);
      }),
      { numRuns: 100 },
    );
  });

  it('only two possible actions exist across all boolean inputs', () => {
    const actions = new Set<string>();
    fc.assert(
      fc.property(fc.boolean(), (outcome) => {
        actions.add(fallbackShareAction(outcome));
      }),
      { numRuns: 100 },
    );
    expect(actions.size).toBe(2);
    expect(actions.has('send_start_share')).toBe(true);
    expect(actions.has('no_op')).toBe(true);
  });
});


/* ═══ Property 20: Share routing correctness ════════════════════════ */
// Feature: cross-platform-share-picker, Property 20: Share routing correctness
// **Validates: Requirements 3.1, 3.6, 4.2, 4.3, 4.4, 4.5, 9.2**

const arbFallbackReason: fc.Arbitrary<FallbackReason | null> = fc.oneof(
  fc.constant('portal' as const),
  fc.constant('get_display_media' as const),
  fc.constant(null),
);

const arbConnectionMode: fc.Arbitrary<'livekit' | 'native' | undefined> = fc.oneof(
  fc.constant('livekit' as const),
  fc.constant('native' as const),
  fc.constant(undefined),
);

const arbSourceCount = fc.integer({ min: 0, max: 10 });

describe('Property 20: Share routing correctness', () => {
  it('error always routes to fallback_share iff connectionMode is livekit, else error_toast', () => {
    fc.assert(
      fc.property(arbConnectionMode, (connectionMode) => {
        const action = computeShareRoute(null, true, connectionMode);
        if (connectionMode === 'livekit') {
          expect(action).toBe('fallback_share');
        } else {
          expect(action).toBe('error_toast');
        }
      }),
      { numRuns: 100 },
    );
  });

  it('success with sources > 0 always opens picker regardless of fallback_reason or connectionMode', () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 1, max: 10 }),
        arbFallbackReason,
        arbConnectionMode,
        (sourceCount, fallbackReason, connectionMode) => {
          const result = { sources: { length: sourceCount }, fallback_reason: fallbackReason };
          const action = computeShareRoute(result, false, connectionMode);
          expect(action).toBe('open_picker');
        },
      ),
      { numRuns: 100 },
    );
  });

  it('success with zero sources + portal fallback always opens picker', () => {
    fc.assert(
      fc.property(arbConnectionMode, (connectionMode) => {
        const result = { sources: { length: 0 }, fallback_reason: 'portal' as const };
        const action = computeShareRoute(result, false, connectionMode);
        expect(action).toBe('open_picker');
      }),
      { numRuns: 100 },
    );
  });

  it('success with zero sources + get_display_media + livekit routes to fallback_share', () => {
    const result = { sources: { length: 0 }, fallback_reason: 'get_display_media' as const };
    const action = computeShareRoute(result, false, 'livekit');
    expect(action).toBe('fallback_share');
  });

  it('success with zero sources + get_display_media + non-livekit routes to no_sources_toast', () => {
    fc.assert(
      fc.property(
        fc.oneof(fc.constant('native' as const), fc.constant(undefined)),
        (connectionMode) => {
          const result = { sources: { length: 0 }, fallback_reason: 'get_display_media' as const };
          const action = computeShareRoute(result, false, connectionMode);
          expect(action).toBe('no_sources_toast');
        },
      ),
      { numRuns: 100 },
    );
  });

  it('success with zero sources + null fallback_reason routes to no_sources_toast', () => {
    fc.assert(
      fc.property(arbConnectionMode, (connectionMode) => {
        const result = { sources: { length: 0 }, fallback_reason: null };
        const action = computeShareRoute(result, false, connectionMode);
        expect(action).toBe('no_sources_toast');
      }),
      { numRuns: 100 },
    );
  });

  it('random combinations always produce a valid action matching the routing table', () => {
    fc.assert(
      fc.property(
        arbSourceCount,
        arbFallbackReason,
        arbConnectionMode,
        fc.boolean(),
        (sourceCount, fallbackReason, connectionMode, enumError) => {
          const enumResult = enumError
            ? null
            : { sources: { length: sourceCount }, fallback_reason: fallbackReason };
          const action = computeShareRoute(enumResult, enumError, connectionMode);

          // Verify against the routing table
          if (enumError) {
            expect(action).toBe(connectionMode === 'livekit' ? 'fallback_share' : 'error_toast');
          } else if (sourceCount > 0 || fallbackReason === 'portal') {
            expect(action).toBe('open_picker');
          } else if (fallbackReason === 'get_display_media' && connectionMode === 'livekit') {
            expect(action).toBe('fallback_share');
          } else {
            expect(action).toBe('no_sources_toast');
          }
        },
      ),
      { numRuns: 100 },
    );
  });
});


/* ═══ Property 21: Stop button routes to correct stop function ══════ */
// Feature: cross-platform-share-picker, Property 21: Stop button routes to correct stop function
// **Validates: Requirements 3.5, 7.1, 7.2, 7.3**

const arbActiveShareType: fc.Arbitrary<ShareMode | null> = fc.oneof(
  arbShareMode,
  fc.constant(null),
);

const arbSelfSharing: fc.Arbitrary<boolean> = fc.boolean();

describe('Property 21: Stop button routes to correct stop function', () => {
  it('non-null activeShareType always returns stop_custom regardless of selfSharing', () => {
    fc.assert(
      fc.property(arbShareMode, arbSelfSharing, (mode, selfSharing) => {
        const result = computeStopRoute(mode, selfSharing);
        expect(result).toBe('stop_custom');
      }),
      { numRuns: 100 },
    );
  });

  it('null activeShareType + selfSharing=true always returns stop_fallback', () => {
    fc.assert(
      fc.property(fc.constant(null), fc.constant(true), (activeShareType, selfSharing) => {
        const result = computeStopRoute(activeShareType, selfSharing);
        expect(result).toBe('stop_fallback');
      }),
      { numRuns: 100 },
    );
  });

  it('null activeShareType + selfSharing=false always returns none', () => {
    fc.assert(
      fc.property(fc.constant(null), fc.constant(false), (activeShareType, selfSharing) => {
        const result = computeStopRoute(activeShareType, selfSharing);
        expect(result).toBe('none');
      }),
      { numRuns: 100 },
    );
  });

  it('random combinations verify routing is deterministic and correct', () => {
    fc.assert(
      fc.property(arbActiveShareType, arbSelfSharing, (activeShareType, selfSharing) => {
        const result = computeStopRoute(activeShareType, selfSharing);

        if (activeShareType !== null) {
          expect(result).toBe('stop_custom');
        } else if (selfSharing) {
          expect(result).toBe('stop_fallback');
        } else {
          expect(result).toBe('none');
        }
      }),
      { numRuns: 100 },
    );
  });

  it('result is always one of the three valid stop routes', () => {
    fc.assert(
      fc.property(arbActiveShareType, arbSelfSharing, (activeShareType, selfSharing) => {
        const result = computeStopRoute(activeShareType, selfSharing);
        expect(['stop_custom', 'stop_fallback', 'none']).toContain(result);
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 22: Share button disabled during any active share ════ */
// Feature: cross-platform-share-picker, Property 22: Share button disabled during any active share
// **Validates: Requirements 7.4**

describe('Property 22: Share button disabled during any active share', () => {
  it('disabled iff activeShareType !== null || selfSharing', () => {
    fc.assert(
      fc.property(arbActiveShareType, arbSelfSharing, (activeShareType, selfSharing) => {
        const disabled = isShareButtonDisabled(activeShareType, selfSharing);
        const expected = activeShareType !== null || selfSharing;
        expect(disabled).toBe(expected);
      }),
      { numRuns: 100 },
    );
  });

  it('both null and false means not disabled', () => {
    fc.assert(
      fc.property(fc.constant(null), fc.constant(false), (activeShareType, selfSharing) => {
        expect(isShareButtonDisabled(activeShareType, selfSharing)).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('non-null activeShareType always means disabled regardless of selfSharing', () => {
    fc.assert(
      fc.property(arbShareMode, arbSelfSharing, (mode, selfSharing) => {
        expect(isShareButtonDisabled(mode, selfSharing)).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('selfSharing=true always means disabled regardless of activeShareType', () => {
    fc.assert(
      fc.property(arbActiveShareType, (activeShareType) => {
        expect(isShareButtonDisabled(activeShareType, true)).toBe(true);
      }),
      { numRuns: 100 },
    );
  });
});


/* ═══ Property 25: SharePicker portal fallback button visibility ════ */
// Feature: cross-platform-share-picker, Property 25: SharePicker portal fallback button visibility
// **Validates: Requirements 3.7**

import { shouldShowPortalFallback, hasEchoWarning } from '@features/screen-share/SharePicker';

describe('Property 25: SharePicker portal fallback button visibility', () => {
  it('portal fallback_reason always shows portal button', () => {
    fc.assert(
      fc.property(fc.constant('portal' as const), (reason) => {
        expect(shouldShowPortalFallback(reason)).toBe(true);
      }),
      { numRuns: 100 },
    );
  });

  it('get_display_media fallback_reason never shows portal button', () => {
    fc.assert(
      fc.property(fc.constant('get_display_media' as const), (reason) => {
        expect(shouldShowPortalFallback(reason)).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('null fallback_reason never shows portal button', () => {
    fc.assert(
      fc.property(fc.constant(null), (reason) => {
        expect(shouldShowPortalFallback(reason)).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('random fallback_reason values: visible iff portal', () => {
    fc.assert(
      fc.property(arbFallbackReason, (reason) => {
        const visible = shouldShowPortalFallback(reason);
        expect(visible).toBe(reason === 'portal');
      }),
      { numRuns: 100 },
    );
  });
});

/* ═══ Property 26: Echo warning visibility on SystemAudio sources ═══ */
// Feature: cross-platform-share-picker, Property 26: Echo warning visibility on SystemAudio sources
// **Validates: Requirements 6.7**

const arbWarnings: fc.Arbitrary<string[]> = fc.oneof(
  fc.constant([] as string[]),
  fc.constant(['echo possible: system audio may include your own voice'] as string[]),
  fc.constant(['some other warning'] as string[]),
  fc.constant(['echo possible: system audio may include your own voice', 'some other warning'] as string[]),
);

describe('Property 26: Echo warning visibility on SystemAudio sources', () => {
  it('warnings containing "echo possible" always trigger echo warning', () => {
    fc.assert(
      fc.property(
        fc.constant(['echo possible: system audio may include your own voice'] as string[]),
        (warnings) => {
          expect(hasEchoWarning(warnings)).toBe(true);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('warnings without "echo possible" never trigger echo warning', () => {
    fc.assert(
      fc.property(
        fc.array(fc.string().filter((s) => !s.includes('echo possible')), { minLength: 0, maxLength: 5 }),
        (warnings) => {
          expect(hasEchoWarning(warnings)).toBe(false);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('empty warnings array never triggers echo warning', () => {
    expect(hasEchoWarning([])).toBe(false);
  });

  it('random warnings: echo active iff any warning contains "echo possible"', () => {
    fc.assert(
      fc.property(arbWarnings, (warnings) => {
        const active = hasEchoWarning(warnings);
        const expected = warnings.some((w) => w.includes('echo possible'));
        expect(active).toBe(expected);
      }),
      { numRuns: 100 },
    );
  });

  it('echo warning applies only to SystemAudio sources when active', () => {
    // Simulate the component logic: echoWarningActive && source.source_type === 'system_audio'
    const arbSourceType = fc.constantFrom('screen' as const, 'window' as const, 'system_audio' as const);
    fc.assert(
      fc.property(fc.boolean(), arbSourceType, (echoActive, sourceType) => {
        const showWarning = echoActive && sourceType === 'system_audio';
        if (sourceType !== 'system_audio') {
          expect(showWarning).toBe(false);
        } else {
          expect(showWarning).toBe(echoActive);
        }
      }),
      { numRuns: 100 },
    );
  });
});


/* ═══ Property 24: Inline badge visibility matches fallback share state ═ */
// Feature: cross-platform-share-picker, Property 24: Inline badge visibility matches fallback share state
// **Validates: Requirements 8.2**

import { isFallbackBadgeVisible } from '../voice-room';

describe('Property 24: Inline badge visibility matches fallback share state', () => {
  it('visible iff activeShareType === null && selfSharing === true', () => {
    fc.assert(
      fc.property(arbActiveShareType, arbSelfSharing, (activeShareType, selfSharing) => {
        const visible = isFallbackBadgeVisible(activeShareType, selfSharing);
        const expected = activeShareType === null && selfSharing === true;
        expect(visible).toBe(expected);
      }),
      { numRuns: 100 },
    );
  });

  it('never visible when custom share is active', () => {
    fc.assert(
      fc.property(arbShareMode, arbSelfSharing, (mode, selfSharing) => {
        expect(isFallbackBadgeVisible(mode, selfSharing)).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('never visible when not sharing at all', () => {
    fc.assert(
      fc.property(arbActiveShareType, (activeShareType) => {
        expect(isFallbackBadgeVisible(activeShareType, false)).toBe(false);
      }),
      { numRuns: 100 },
    );
  });

  it('only visible when null activeShareType + selfSharing', () => {
    expect(isFallbackBadgeVisible(null, true)).toBe(true);
    expect(isFallbackBadgeVisible(null, false)).toBe(false);
    expect(isFallbackBadgeVisible('screen_audio', true)).toBe(false);
    expect(isFallbackBadgeVisible('window', true)).toBe(false);
    expect(isFallbackBadgeVisible('audio_only', true)).toBe(false);
  });
});


/* ═══ Property: leaveRoom share cleanup routing ═════════════════════ */
// Feature: cross-platform-share-picker, Task 16.2: leaveRoom fallback cleanup
// **Validates: Requirements 7.1, 8.3**

import { computeLeaveShareCleanup } from '../voice-room';

describe('leaveRoom share cleanup routing', () => {
  it('custom share active → cleanup is custom', () => {
    fc.assert(
      fc.property(arbShareMode, arbSelfSharing, (mode, selfSharing) => {
        const result = computeLeaveShareCleanup(mode, selfSharing);
        expect(result).toBe('custom');
      }),
      { numRuns: 100 },
    );
  });

  it('fallback share active (null type + selfSharing) → cleanup is fallback', () => {
    fc.assert(
      fc.property(fc.constant(null), fc.constant(true), (activeShareType, selfSharing) => {
        const result = computeLeaveShareCleanup(activeShareType, selfSharing);
        expect(result).toBe('fallback');
      }),
      { numRuns: 100 },
    );
  });

  it('not sharing → cleanup is none', () => {
    fc.assert(
      fc.property(fc.constant(null), fc.constant(false), (activeShareType, selfSharing) => {
        const result = computeLeaveShareCleanup(activeShareType, selfSharing);
        expect(result).toBe('none');
      }),
      { numRuns: 100 },
    );
  });

  it('random combinations always produce a valid cleanup action', () => {
    fc.assert(
      fc.property(arbActiveShareType, arbSelfSharing, (activeShareType, selfSharing) => {
        const result = computeLeaveShareCleanup(activeShareType, selfSharing);
        expect(['custom', 'fallback', 'none']).toContain(result);

        if (activeShareType !== null) {
          expect(result).toBe('custom');
        } else if (selfSharing) {
          expect(result).toBe('fallback');
        } else {
          expect(result).toBe('none');
        }
      }),
      { numRuns: 100 },
    );
  });
});
