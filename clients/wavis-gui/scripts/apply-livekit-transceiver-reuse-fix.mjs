import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const LIVEKIT_FIX_LOG_MARKERS = [
  '[livekit-fix] sender.setParameters attempt',
  '[livekit-fix] sender.setParameters success',
  '[livekit-fix] sender.setParameters failed',
];

export const DEGRADATION_PREF_MARKERS = [
  '[livekit-fix] degradationPreference.setParameters attempt',
  '[livekit-fix] degradationPreference.setParameters success',
  '[livekit-fix] degradationPreference.setParameters failed',
  '[livekit-fix] degradationPreference.setParameters skipped_reused_sender',
  '[livekit-fix] setDegradationPreference skipped (already configured via combined call)',
];

export const REQUIRED_POST_PATCH_MARKERS = [
  '[livekit-fix] reuse inactive transceiver',
  '[livekit-fix] sender.setParameters attempt',
  '[livekit-fix] degradationPreference.setParameters attempt',
  '[livekit-fix] degradationPreference.setParameters skipped_reused_sender',
  '[livekit-fix] post-replaceTrack combined setParameters success',
  '[livekit-fix] setDegradationPreference skipped (already configured via combined call)',
  '__wavisSenderData',
];

// ── Transceiver reuse anchors ──────────────────────────────────────────
// Each anchor represents a known state of the bundle. The patch script
// recognises all of them and upgrades to createTransceiverAfter.

const createTransceiverBefore = `  createTransceiverRTCRtpSender(track, opts, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        throw new UnexpectedConnectionState('publisher is closed');
      }
      const streams = [];
      if (track.mediaStream) {
        streams.push(track.mediaStream);
      }
      if (isVideoTrack(track)) {
        track.codec = opts.videoCodec;
      }
      const transceiverInit = {
        direction: 'sendonly',
        streams
      };
      if (encodings) {
        transceiverInit.sendEncodings = encodings;
      }
      // addTransceiver for react-native is async. web is synchronous, but await won't effect it.
      const transceiver = yield this.pcManager.addPublisherTransceiver(track.mediaStreamTrack, transceiverInit);
      return transceiver.sender;
    });
  }
`;

const createTransceiverPatchedWithoutLogs = `  createTransceiverRTCRtpSender(track, opts, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        throw new UnexpectedConnectionState('publisher is closed');
      }
      const streams = [];
      if (track.mediaStream) {
        streams.push(track.mediaStream);
      }
      if (isVideoTrack(track)) {
        track.codec = opts.videoCodec;
      }
      const reusedSender = yield this.tryReuseInactivePublisherSender(track, encodings);
      if (reusedSender) {
        return reusedSender;
      }
      const transceiverInit = {
        direction: 'sendonly',
        streams
      };
      if (encodings) {
        transceiverInit.sendEncodings = encodings;
      }
      // addTransceiver for react-native is async. web is synchronous, but await won't effect it.
      const transceiver = yield this.pcManager.addPublisherTransceiver(track.mediaStreamTrack, transceiverInit);
      return transceiver.sender;
    });
  }
  tryReuseInactivePublisherSender(track, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        return void 0;
      }
      const transceiver = this.pcManager.publisher.getTransceivers().find(tr => {
        var _a;
        const isStopped = typeof tr.stopped === 'boolean' ? tr.stopped : false;
        return !isStopped && tr.sender.track === null && ((_a = tr.receiver.track) === null || _a === void 0 ? void 0 : _a.kind) === track.kind && tr.direction === 'inactive';
      });
      if (!transceiver) {
        return void 0;
      }
      transceiver.direction = 'sendonly';
      if (encodings) {
        try {
          const parameters = transceiver.sender.getParameters();
          parameters.encodings = encodings;
          yield transceiver.sender.setParameters(parameters);
        } catch (error) {
          this.log.warn('could not apply sender parameters while reusing transceiver', Object.assign(Object.assign({}, this.logContext), { error }));
        }
      }
      yield transceiver.sender.replaceTrack(track.mediaStreamTrack);
      return transceiver.sender;
    });
  }
`;

const createTransceiverPatchedWithLogs = `  createTransceiverRTCRtpSender(track, opts, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        throw new UnexpectedConnectionState('publisher is closed');
      }
      const streams = [];
      if (track.mediaStream) {
        streams.push(track.mediaStream);
      }
      if (isVideoTrack(track)) {
        track.codec = opts.videoCodec;
      }
      const transceiverCountBeforeAdd = this.pcManager.publisher.getTransceivers().length;
      const reusedSender = yield this.tryReuseInactivePublisherSender(track, encodings);
      if (reusedSender) {
        console.log('[livekit-fix] reused sender', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return reusedSender;
      }
      console.log('[livekit-fix] creating new transceiver', {
        kind: track.kind,
        transceiverCountBeforeAdd
      });
      const transceiverInit = {
        direction: 'sendonly',
        streams
      };
      if (encodings) {
        transceiverInit.sendEncodings = encodings;
      }
      // addTransceiver for react-native is async. web is synchronous, but await won't effect it.
      const transceiver = yield this.pcManager.addPublisherTransceiver(track.mediaStreamTrack, transceiverInit);
      console.log('[livekit-fix] created transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        transceiverCountAfterAdd: this.pcManager.publisher.getTransceivers().length
      });
      return transceiver.sender;
    });
  }
  tryReuseInactivePublisherSender(track, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        return void 0;
      }
      const transceiver = this.pcManager.publisher.getTransceivers().find(tr => {
        var _a;
        const isStopped = typeof tr.stopped === 'boolean' ? tr.stopped : false;
        return !isStopped && tr.sender.track === null && ((_a = tr.receiver.track) === null || _a === void 0 ? void 0 : _a.kind) === track.kind && tr.direction === 'inactive';
      });
      if (!transceiver) {
        console.log('[livekit-fix] no reusable inactive transceiver', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return void 0;
      }
      console.log('[livekit-fix] reuse inactive transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        direction: transceiver.direction,
        currentDirection: transceiver.currentDirection,
        transceiverCount: this.pcManager.publisher.getTransceivers().length
      });
      transceiver.direction = 'sendonly';
      if (encodings) {
        try {
          const parameters = transceiver.sender.getParameters();
          parameters.encodings = encodings;
          yield transceiver.sender.setParameters(parameters);
        } catch (error) {
          this.log.warn('could not apply sender parameters while reusing transceiver', Object.assign(Object.assign({}, this.logContext), { error }));
        }
      }
      yield transceiver.sender.replaceTrack(track.mediaStreamTrack);
      return transceiver.sender;
    });
  }
`;

// Intermediate state: has crash guard + reuse logs but lacks detailed
// sender.setParameters diagnostics and replaceTrack logging.
const createTransceiverPatchedCrashGuard = `  createTransceiverRTCRtpSender(track, opts, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        throw new UnexpectedConnectionState('publisher is closed');
      }
      const streams = [];
      if (track.mediaStream) {
        streams.push(track.mediaStream);
      }
      if (isVideoTrack(track)) {
        track.codec = opts.videoCodec;
      }
      const transceiverCountBeforeAdd = this.pcManager.publisher.getTransceivers().length;
      let reusedSender;
      try {
        reusedSender = yield this.tryReuseInactivePublisherSender(track, encodings);
      } catch (error) {
        console.warn('[livekit-fix] reuse attempt crashed, falling back to new transceiver', {
          kind: track.kind,
          error: error instanceof Error ? error.message : String(error)
        });
        reusedSender = void 0;
      }
      if (reusedSender) {
        console.log('[livekit-fix] reused sender', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return reusedSender;
      }
      console.log('[livekit-fix] creating new transceiver', {
        kind: track.kind,
        transceiverCountBeforeAdd
      });
      const transceiverInit = {
        direction: 'sendonly',
        streams
      };
      if (encodings) {
        transceiverInit.sendEncodings = encodings;
      }
      // addTransceiver for react-native is async. web is synchronous, but await won't effect it.
      const transceiver = yield this.pcManager.addPublisherTransceiver(track.mediaStreamTrack, transceiverInit);
      console.log('[livekit-fix] created transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        transceiverCountAfterAdd: this.pcManager.publisher.getTransceivers().length
      });
      return transceiver.sender;
    });
  }
  tryReuseInactivePublisherSender(track, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        return void 0;
      }
      const transceiver = this.pcManager.publisher.getTransceivers().find(tr => {
        var _a;
        const isStopped = typeof tr.stopped === 'boolean' ? tr.stopped : false;
        return !isStopped && tr.sender.track === null && ((_a = tr.receiver.track) === null || _a === void 0 ? void 0 : _a.kind) === track.kind && tr.direction === 'inactive';
      });
      if (!transceiver) {
        console.log('[livekit-fix] no reusable inactive transceiver', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return void 0;
      }
      console.log('[livekit-fix] reuse inactive transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        direction: transceiver.direction,
        currentDirection: transceiver.currentDirection,
        transceiverCount: this.pcManager.publisher.getTransceivers().length
      });
      try {
        transceiver.direction = 'sendonly';
        if (encodings) {
          try {
            const parameters = transceiver.sender.getParameters();
            parameters.encodings = encodings;
            yield transceiver.sender.setParameters(parameters);
          } catch (error) {
            this.log.warn('could not apply sender parameters while reusing transceiver', Object.assign(Object.assign({}, this.logContext), { error }));
          }
        }
        yield transceiver.sender.replaceTrack(track.mediaStreamTrack);
        return transceiver.sender;
      } catch (error) {
        console.warn('[livekit-fix] reuse inactive transceiver failed', {
          kind: track.kind,
          mid: transceiver.mid,
          error: error instanceof Error ? error.message : String(error)
        });
        try {
          transceiver.direction = 'inactive';
        } catch (_error) {}
        return void 0;
      }
    });
  }
`;

const createTransceiverPatchedTracklessReuseMarker = `  createTransceiverRTCRtpSender(track, opts, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        throw new UnexpectedConnectionState('publisher is closed');
      }
      const streams = [];
      if (track.mediaStream) {
        streams.push(track.mediaStream);
      }
      if (isVideoTrack(track)) {
        track.codec = opts.videoCodec;
      }
      const transceiverCountBeforeAdd = this.pcManager.publisher.getTransceivers().length;
      let reusedSender;
      try {
        reusedSender = yield this.tryReuseInactivePublisherSender(track, encodings);
      } catch (error) {
        console.warn('[livekit-fix] reuse attempt crashed, falling back to new transceiver', {
          kind: track.kind,
          error: error instanceof Error ? error.message : String(error)
        });
        reusedSender = void 0;
      }
      if (reusedSender) {
        console.log('[livekit-fix] reused sender', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return reusedSender;
      }
      console.log('[livekit-fix] creating new transceiver', {
        kind: track.kind,
        transceiverCountBeforeAdd
      });
      const transceiverInit = {
        direction: 'sendonly',
        streams
      };
      if (encodings) {
        transceiverInit.sendEncodings = encodings;
      }
      // addTransceiver for react-native is async. web is synchronous, but await won't effect it.
      const transceiver = yield this.pcManager.addPublisherTransceiver(track.mediaStreamTrack, transceiverInit);
      console.log('[livekit-fix] created transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        transceiverCountAfterAdd: this.pcManager.publisher.getTransceivers().length
      });
      return transceiver.sender;
    });
  }
  tryReuseInactivePublisherSender(track, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        return void 0;
      }
      const transceiver = this.pcManager.publisher.getTransceivers().find(tr => {
        var _a;
        const isStopped = typeof tr.stopped === 'boolean' ? tr.stopped : false;
        return !isStopped && tr.sender.track === null && ((_a = tr.receiver.track) === null || _a === void 0 ? void 0 : _a.kind) === track.kind && tr.direction === 'inactive';
      });
      if (!transceiver) {
        console.log('[livekit-fix] no reusable inactive transceiver', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return void 0;
      }
      console.log('[livekit-fix] reuse inactive transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        direction: transceiver.direction,
        currentDirection: transceiver.currentDirection,
        transceiverCount: this.pcManager.publisher.getTransceivers().length
      });
      try {
        transceiver.direction = 'sendonly';
        if (encodings) {
          const senderTrackIdBeforeReplace = transceiver.sender.track ? transceiver.sender.track.id : null;
          let getParametersCalled = false;
          let existingEncodingCount = 0;
          try {
            getParametersCalled = true;
            const parameters = transceiver.sender.getParameters();
            existingEncodingCount = Array.isArray(parameters.encodings) ? parameters.encodings.length : 0;
            console.log('[livekit-fix] sender.setParameters attempt', {
              kind: track.kind,
              mid: transceiver.mid,
              direction: transceiver.direction,
              currentDirection: transceiver.currentDirection,
              senderTrackIdBeforeReplace,
              getParametersCalled,
              existingEncodingCount,
              requestedEncodingCount: Array.isArray(encodings) ? encodings.length : 0
            });
            parameters.encodings = encodings;
            yield transceiver.sender.setParameters(parameters);
            console.log('[livekit-fix] sender.setParameters success', {
              kind: track.kind,
              mid: transceiver.mid,
              senderTrackIdBeforeReplace,
              appliedEncodingCount: Array.isArray(encodings) ? encodings.length : 0
            });
          } catch (error) {
            const errorName = error && typeof error === 'object' && 'name' in error ? error.name : null;
            const errorMessage = error instanceof Error ? error.message : String(error);
            console.warn('[livekit-fix] sender.setParameters failed', {
              kind: track.kind,
              mid: transceiver.mid,
              direction: transceiver.direction,
              currentDirection: transceiver.currentDirection,
              senderTrackIdBeforeReplace,
              getParametersCalled,
              existingEncodingCount,
              requestedEncodingCount: Array.isArray(encodings) ? encodings.length : 0,
              errorName,
              errorMessage
            });
            this.log.warn('could not apply sender parameters while reusing transceiver', Object.assign(Object.assign({}, this.logContext), { error }));
          }
        }
        const senderTrackIdBeforeReplace = transceiver.sender.track ? transceiver.sender.track.id : null;
        yield transceiver.sender.replaceTrack(track.mediaStreamTrack);
        console.log('[livekit-fix] sender.replaceTrack success', {
          kind: track.kind,
          mid: transceiver.mid,
          senderTrackIdBeforeReplace,
          senderTrackIdAfterReplace: transceiver.sender.track ? transceiver.sender.track.id : null
        });
        if (track.kind === 'video') {
          transceiver.sender.__wavisReusedSender = true;
          transceiver.sender.__wavisDegradationPreferenceAttempts = [];
          transceiver.sender.__wavisDegradationPreferenceInvalidStateSkipped = false;
          transceiver.sender.__wavisDegradationPreferenceLastErrorName = null;
          transceiver.sender.__wavisDegradationPreferenceLastErrorMessage = null;
        }
        return transceiver.sender;
      } catch (error) {
        console.warn('[livekit-fix] reuse inactive transceiver failed', {
          kind: track.kind,
          mid: transceiver.mid,
          error: error instanceof Error ? error.message : String(error)
        });
        try {
          transceiver.direction = 'inactive';
        } catch (_error) {}
        return void 0;
      }
    });
  }
`;

const createTransceiverAfter = `  createTransceiverRTCRtpSender(track, opts, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        throw new UnexpectedConnectionState('publisher is closed');
      }
      const streams = [];
      if (track.mediaStream) {
        streams.push(track.mediaStream);
      }
      if (isVideoTrack(track)) {
        track.codec = opts.videoCodec;
      }
      const transceiverCountBeforeAdd = this.pcManager.publisher.getTransceivers().length;
      let reusedSender;
      try {
        reusedSender = yield this.tryReuseInactivePublisherSender(track, encodings);
      } catch (error) {
        console.warn('[livekit-fix] reuse attempt crashed, falling back to new transceiver', {
          kind: track.kind,
          error: error instanceof Error ? error.message : String(error)
        });
        reusedSender = void 0;
      }
      if (reusedSender) {
        console.log('[livekit-fix] reused sender', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return reusedSender;
      }
      console.log('[livekit-fix] creating new transceiver', {
        kind: track.kind,
        transceiverCountBeforeAdd
      });
      const transceiverInit = {
        direction: 'sendonly',
        streams
      };
      if (encodings) {
        transceiverInit.sendEncodings = encodings;
      }
      // addTransceiver for react-native is async. web is synchronous, but await won't effect it.
      const transceiver = yield this.pcManager.addPublisherTransceiver(track.mediaStreamTrack, transceiverInit);
      console.log('[livekit-fix] created transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        transceiverCountAfterAdd: this.pcManager.publisher.getTransceivers().length
      });
      return transceiver.sender;
    });
  }
  tryReuseInactivePublisherSender(track, encodings) {
    return __awaiter(this, void 0, void 0, function* () {
      if (!this.pcManager) {
        return void 0;
      }
      const transceiver = this.pcManager.publisher.getTransceivers().find(tr => {
        var _a;
        const isStopped = typeof tr.stopped === 'boolean' ? tr.stopped : false;
        return !isStopped && tr.sender.track === null && ((_a = tr.receiver.track) === null || _a === void 0 ? void 0 : _a.kind) === track.kind && tr.direction === 'inactive';
      });
      if (!transceiver) {
        console.log('[livekit-fix] no reusable inactive transceiver', {
          kind: track.kind,
          transceiverCount: this.pcManager.publisher.getTransceivers().length
        });
        return void 0;
      }
      console.log('[livekit-fix] reuse inactive transceiver', {
        kind: track.kind,
        mid: transceiver.mid,
        direction: transceiver.direction,
        currentDirection: transceiver.currentDirection,
        transceiverCount: this.pcManager.publisher.getTransceivers().length
      });
      try {
        transceiver.direction = 'sendonly';
        if (encodings) {
          const senderTrackIdBeforeReplace = transceiver.sender.track ? transceiver.sender.track.id : null;
          let getParametersCalled = false;
          let existingEncodingCount = 0;
          try {
            getParametersCalled = true;
            const parameters = transceiver.sender.getParameters();
            existingEncodingCount = Array.isArray(parameters.encodings) ? parameters.encodings.length : 0;
            console.log('[livekit-fix] sender.setParameters attempt', {
              kind: track.kind,
              mid: transceiver.mid,
              direction: transceiver.direction,
              currentDirection: transceiver.currentDirection,
              senderTrackIdBeforeReplace,
              getParametersCalled,
              existingEncodingCount,
              requestedEncodingCount: Array.isArray(encodings) ? encodings.length : 0
            });
            parameters.encodings = encodings;
            yield transceiver.sender.setParameters(parameters);
            console.log('[livekit-fix] sender.setParameters success', {
              kind: track.kind,
              mid: transceiver.mid,
              senderTrackIdBeforeReplace,
              appliedEncodingCount: Array.isArray(encodings) ? encodings.length : 0
            });
          } catch (error) {
            const errorName = error && typeof error === 'object' && 'name' in error ? error.name : null;
            const errorMessage = error instanceof Error ? error.message : String(error);
            console.warn('[livekit-fix] sender.setParameters failed', {
              kind: track.kind,
              mid: transceiver.mid,
              direction: transceiver.direction,
              currentDirection: transceiver.currentDirection,
              senderTrackIdBeforeReplace,
              getParametersCalled,
              existingEncodingCount,
              requestedEncodingCount: Array.isArray(encodings) ? encodings.length : 0,
              errorName,
              errorMessage
            });
            this.log.warn('could not apply sender parameters while reusing transceiver', Object.assign(Object.assign({}, this.logContext), { error }));
          }
        }
        const senderTrackIdBeforeReplace = transceiver.sender.track ? transceiver.sender.track.id : null;
        yield transceiver.sender.replaceTrack(track.mediaStreamTrack);
        console.log('[livekit-fix] sender.replaceTrack success', {
          kind: track.kind,
          mid: transceiver.mid,
          senderTrackIdBeforeReplace,
          senderTrackIdAfterReplace: transceiver.sender.track ? transceiver.sender.track.id : null
        });
        if (track.kind === 'video') {
          const wavisGlobal = typeof window !== 'undefined' ? window : globalThis;
          if (!wavisGlobal.__wavisSenderData) {
            wavisGlobal.__wavisSenderData = new WeakMap();
            const __testKey = {};
            wavisGlobal.__wavisSenderData.set(__testKey, { _test: true });
            console.assert((wavisGlobal.__wavisSenderData.get(__testKey) || {})._test === true, '[livekit-fix] WeakMap side channel OK');
            wavisGlobal.__wavisSenderData.delete(__testKey);
          }
          const senderDataStore = wavisGlobal.__wavisSenderData;
          senderDataStore.set(transceiver.sender, {
            reused: true,
            degradationPreferenceConfigured: false,
            attemptedPreferences: [],
            invalidStateSkipped: false,
            lastErrorName: null,
            lastErrorMessage: null
          });
          try {
            const postReplaceParams = transceiver.sender.getParameters();
            const postReplaceEncodingCount = Array.isArray(postReplaceParams.encodings) ? postReplaceParams.encodings.length : 0;
            if (Array.isArray(encodings) && encodings.length > 0 && postReplaceEncodingCount === 0) {
              postReplaceParams.encodings = encodings;
            }
            postReplaceParams.degradationPreference = 'maintain-resolution';
            yield transceiver.sender.setParameters(postReplaceParams);
            const senderData = senderDataStore.get(transceiver.sender);
            if (senderData) {
              senderData.degradationPreferenceConfigured = true;
              senderData.attemptedPreferences.push('maintain-resolution-combined');
            }
            console.log('[livekit-fix] post-replaceTrack combined setParameters success', {
              senderTrackId: transceiver.sender.track ? transceiver.sender.track.id : null
            });
          } catch (e) {
            console.warn('[livekit-fix] post-replaceTrack combined setParameters failed', {
              error: e
            });
          }
        }
        return transceiver.sender;
      } catch (error) {
        console.warn('[livekit-fix] reuse inactive transceiver failed', {
          kind: track.kind,
          mid: transceiver.mid,
          error: error instanceof Error ? error.message : String(error)
        });
        try {
          transceiver.direction = 'inactive';
        } catch (_error) {}
        return void 0;
      }
    });
  }
`;

// ── Degradation preference anchors ─────────────────────────────────────
// The original setDegradationPreference does NOT yield the setParameters
// call, so the returned promise rejection is uncaught. The patch adds
// yield + [livekit-fix] logging.

const degradationPrefBefore = `setDegradationPreference(preference) {
    return __awaiter(this, void 0, void 0, function* () {
      this.degradationPreference = preference;
      if (this.sender) {
        try {
          this.log.debug("setting degradationPreference to ".concat(preference), this.logContext);
          const params = this.sender.getParameters();
          params.degradationPreference = preference;
          this.sender.setParameters(params);
        } catch (e) {
          this.log.warn("failed to set degradationPreference", Object.assign({
            error: e
          }, this.logContext));
        }
      }
    });
  }`;

const degradationPrefAfter = `setDegradationPreference(preference) {
    return __awaiter(this, void 0, void 0, function* () {
      this.degradationPreference = preference;
      if (this.sender) {
        try {
          this.log.debug("setting degradationPreference to ".concat(preference), this.logContext);
          const wavisGlobal = typeof window !== 'undefined' ? window : globalThis;
          const senderDataStore = wavisGlobal.__wavisSenderData;
          const senderData = senderDataStore ? senderDataStore.get(this.sender) : void 0;
          if (!senderData) {
            return;
          }
          if (senderData.degradationPreferenceConfigured) {
            console.log('[livekit-fix] setDegradationPreference skipped (already configured via combined call)', {
              preference,
              senderTrackId: this.sender && this.sender.track ? this.sender.track.id : null
            });
            return;
          }
          const senderWasReused = senderData.reused === true;
          senderData.attemptedPreferences.push(preference);
          senderData.lastErrorName = null;
          senderData.lastErrorMessage = null;
          const params = this.sender.getParameters();
          params.degradationPreference = preference;
          console.log('[livekit-fix] degradationPreference.setParameters attempt', {
            preference,
            senderTrackId: this.sender.track ? this.sender.track.id : null,
            senderWasReused
          });
          yield this.sender.setParameters(params);
          console.log('[livekit-fix] degradationPreference.setParameters success', {
            preference,
            senderTrackId: this.sender.track ? this.sender.track.id : null,
            senderWasReused
          });
        } catch (e) {
          const wavisGlobal = typeof window !== 'undefined' ? window : globalThis;
          const senderDataStore = wavisGlobal.__wavisSenderData;
          const senderData = senderDataStore ? senderDataStore.get(this.sender) : void 0;
          const senderWasReused = (senderData === null || senderData === void 0 ? void 0 : senderData.reused) === true;
          const errorName = e && typeof e === 'object' && 'name' in e ? e.name : null;
          const errorMessage = e instanceof Error ? e.message : String(e);
          if (senderData) {
            senderData.lastErrorName = errorName;
            senderData.lastErrorMessage = errorMessage;
          }
          if (senderWasReused && errorName === 'InvalidStateError') {
            if (senderData) {
              senderData.invalidStateSkipped = true;
            }
            console.warn('[livekit-fix] degradationPreference.setParameters skipped_reused_sender', {
              preference,
              senderTrackId: this.sender.track ? this.sender.track.id : null,
              errorName,
              errorMessage
            });
            return;
          }
          console.warn('[livekit-fix] degradationPreference.setParameters failed', {
            preference,
            senderTrackId: this.sender ? (this.sender.track ? this.sender.track.id : null) : null,
            senderWasReused,
            errorName,
            errorMessage
          });
          this.log.warn("failed to set degradationPreference", Object.assign({
            error: e
          }, this.logContext));
        }
      }
    });
  }`;

const degradationPrefPatchedWithoutReuseTrackState = `setDegradationPreference(preference) {
    return __awaiter(this, void 0, void 0, function* () {
      this.degradationPreference = preference;
      if (this.sender) {
        try {
          this.log.debug("setting degradationPreference to ".concat(preference), this.logContext);
          const params = this.sender.getParameters();
          params.degradationPreference = preference;
          console.log('[livekit-fix] degradationPreference.setParameters attempt', {
            preference,
            senderTrackId: this.sender.track ? this.sender.track.id : null
          });
          yield this.sender.setParameters(params);
          console.log('[livekit-fix] degradationPreference.setParameters success', {
            preference,
            senderTrackId: this.sender.track ? this.sender.track.id : null
          });
        } catch (e) {
          console.warn('[livekit-fix] degradationPreference.setParameters failed', {
            preference,
            senderTrackId: this.sender ? (this.sender.track ? this.sender.track.id : null) : null,
            errorName: e && typeof e === 'object' && 'name' in e ? e.name : null,
            errorMessage: e instanceof Error ? e.message : String(e)
          });
          this.log.warn("failed to set degradationPreference", Object.assign({
            error: e
          }, this.logContext));
        }
      }
    });
  }`;

// ── Test fixtures ──────────────────────────────────────────────────────

export const livekitFixTestFixtures = {
  before: createTransceiverBefore,
  patchedWithoutLogs: createTransceiverPatchedWithoutLogs,
  patchedWithLogs: createTransceiverPatchedWithLogs,
  patchedCrashGuard: createTransceiverPatchedCrashGuard,
  patchedTracklessReuseMarker: createTransceiverPatchedTracklessReuseMarker,
  degradationPrefBefore,
  degradationPrefPatchedWithoutReuseTrackState,
  degradationPrefAfter,
};

// ── Patch functions ────────────────────────────────────────────────────

function replaceMethodBlock(esm, startMarker, endMarker, replacement) {
  const startIndex = esm.indexOf(startMarker);
  if (startIndex === -1) {
    return null;
  }
  const endIndex = esm.indexOf(endMarker, startIndex);
  if (endIndex === -1) {
    return null;
  }
  return esm.slice(0, startIndex) + replacement + esm.slice(endIndex);
}

export function applyTransceiverReusePatch(esm) {
  // Already at the latest version
  if (esm.includes(createTransceiverAfter)) {
    return esm;
  }
  if (esm.includes(createTransceiverPatchedCrashGuard)) {
    return esm.replace(createTransceiverPatchedCrashGuard, createTransceiverAfter);
  }
  if (esm.includes(createTransceiverPatchedTracklessReuseMarker)) {
    return esm.replace(createTransceiverPatchedTracklessReuseMarker, createTransceiverAfter);
  }
  if (esm.includes(createTransceiverPatchedWithLogs)) {
    return esm.replace(createTransceiverPatchedWithLogs, createTransceiverAfter);
  }
  if (esm.includes(createTransceiverPatchedWithoutLogs)) {
    return esm.replace(createTransceiverPatchedWithoutLogs, createTransceiverAfter);
  }
  if (esm.includes(createTransceiverBefore)) {
    return esm.replace(createTransceiverBefore, createTransceiverAfter);
  }
  const replacedByBoundary = replaceMethodBlock(
    esm,
    '  createTransceiverRTCRtpSender(track, opts, encodings) {',
    '\n  createSimulcastTransceiverSender(track, simulcastTrack, opts, encodings) {',
    `${createTransceiverAfter}\n`,
  );
  if (replacedByBoundary !== null) {
    return replacedByBoundary;
  }
  return null; // anchor not found
}

export function applyDegradationPrefPatch(esm) {
  // Already patched
  if (esm.includes(degradationPrefAfter)) {
    return esm;
  }
  if (esm.includes(degradationPrefPatchedWithoutReuseTrackState)) {
    return esm.replace(degradationPrefPatchedWithoutReuseTrackState, degradationPrefAfter);
  }
  if (esm.includes(degradationPrefBefore)) {
    return esm.replace(degradationPrefBefore, degradationPrefAfter);
  }
  const replacedByBoundary = replaceMethodBlock(
    esm,
    '  setDegradationPreference(preference) {',
    '\n  addSimulcastTrack(codec, encodings) {',
    `${degradationPrefAfter}\n`,
  );
  if (replacedByBoundary !== null) {
    return replacedByBoundary;
  }
  return null; // anchor not found
}

export function applyLivekitTransceiverReuseFix(esm) {
  // Apply transceiver reuse patch
  let result = applyTransceiverReusePatch(esm);
  if (result === null) {
    throw new Error('[livekit-fix] could not find createTransceiverRTCRtpSender anchor in bundle');
  }

  // Apply degradation preference patch
  const degradationResult = applyDegradationPrefPatch(result);
  if (degradationResult === null) {
    throw new Error('[livekit-fix] could not find setDegradationPreference anchor in bundle');
  }
  result = degradationResult;

  return result;
}

export function verifyPatchMarkers(esm) {
  const missing = REQUIRED_POST_PATCH_MARKERS.filter(m => !esm.includes(m));
  if (missing.length > 0) {
    throw new Error(
      `[livekit-fix] post-patch verification failed — missing markers:\n${missing.map(m => `  - ${m}`).join('\n')}`
    );
  }
}

export function patchLivekitBundleFile(esmPath = path.resolve('node_modules/livekit-client/dist/livekit-client.esm.mjs')) {
  if (!fs.existsSync(esmPath)) {
    console.warn('[livekit-fix] skipped: livekit-client ESM bundle not found');
    return false;
  }

  const original = fs.readFileSync(esmPath, 'utf8');
  const patched = applyLivekitTransceiverReuseFix(original);
  if (patched === original) {
    console.log('[livekit-fix] already applied');
    return false;
  }

  // Verify all required markers are present before writing
  verifyPatchMarkers(patched);

  fs.writeFileSync(esmPath, patched, 'utf8');
  console.log('[livekit-fix] applied (transceiver reuse + degradationPreference)');
  return true;
}

const isMainModule =
  process.argv[1] &&
  path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url));

if (isMainModule) {
  patchLivekitBundleFile();
}
