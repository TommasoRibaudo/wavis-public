/**
 * Pure state model of the Detail_Profile adaptive step-down policy.
 * Mirrors the logic in `livekit-media.ts` ADAPTIVE_* constants and
 * `handleAdaptiveQualityStep` / `applyAdaptiveTierConstraints` so that
 * Property 3 can be tested without DOM or WebRTC dependencies.
 *
 * Property 3 invariant: for every step i in Detail_Profile steady state,
 * publishedLayers[i] >= minReadableLayer  OR  telemetryEvents[i] contains 'share.floor.hit'.
 * See design.md §6 Property 3, R16.1, R16.2.
 */
import type { ShareProfile, LayerFloor } from './livekit-media';
import type { TelemetryEvent } from './telemetry';

/* ─── Types ─────────────────────────────────────────────────────── */

/** A single network-condition sample fed to the policy model. */
export interface NetworkSample {
  /** Packet loss percentage 0–100. */
  lossPct: number;
  /** Available bandwidth in kbps. */
  bandwidthKbps: number;
  /** CPU usage percentage 0–100 (currently unused by policy, reserved). */
  cpuPct: number;
  /** Elapsed ms since previous sample (minimum 500 ms at 5-second polling cadence). */
  dtMs: number;
}

/** Instantaneous published encoding layer as the policy model sees it. */
export interface PublishedLayer {
  width: number;
  height: number;
  fps: number;
  /** Estimated bitrate in bps (proportional to area × fps relative to profile maximum). */
  bitrate: number;
}

/** Adaptive tier — mirrors the production `AdaptiveTier` type. */
export type ModelAdaptiveTier = 'full' | 'reduced-fps' | 'reduced-resolution' | 'floor-held';

export interface AdaptationModelState {
  tier: ModelAdaptiveTier;
  consecutiveLossPolls: number;
  consecutiveRecoveryPolls: number;
  consecutiveBandwidthPolls: number;
}

/* ─── Constants (mirrored from livekit-media.ts) ─────────────────── */

const ADAPTIVE_LOSS_THRESHOLD_MODERATE = 5;
const ADAPTIVE_LOSS_THRESHOLD_SEVERE = 15;
const ADAPTIVE_RECOVERY_THRESHOLD = 3;
const ADAPTIVE_STEPDOWN_POLLS = 2;
const ADAPTIVE_RECOVERY_POLLS = 3;
const ADAPTIVE_BANDWIDTH_STEPDOWN_POLLS = 3;

const RESOLUTION_TIERS: ReadonlyArray<{ width: number; height: number }> = [
  { width: 2560, height: 1440 },
  { width: 1920, height: 1080 },
  { width: 1280, height: 720 },
];

const FPS_TIERS: ReadonlyArray<number> = [60, 30, 15];

/* ─── Layer derivation ───────────────────────────────────────────── */

function deriveLayer(tier: ModelAdaptiveTier, profile: ShareProfile): PublishedLayer {
  const fullWidth = profile.resolution.width;
  const fullHeight = profile.resolution.height;
  const fullFps = profile.maxFramerate;
  const fullBitrate = profile.maxBitrate;

  if (tier === 'full') {
    return { width: fullWidth, height: fullHeight, fps: fullFps, bitrate: fullBitrate };
  }

  const baseFpsIdx = FPS_TIERS.indexOf(fullFps);
  const reducedFps =
    baseFpsIdx >= 0 && baseFpsIdx < FPS_TIERS.length - 1
      ? FPS_TIERS[baseFpsIdx + 1]
      : Math.max(15, Math.round(fullFps / 2));

  if (tier === 'reduced-fps' || tier === 'floor-held') {
    const bitrateScale = reducedFps / fullFps;
    return {
      width: fullWidth,
      height: fullHeight,
      fps: reducedFps,
      bitrate: Math.round(fullBitrate * bitrateScale),
    };
  }

  // reduced-resolution
  const baseResIdx = RESOLUTION_TIERS.findIndex(
    (r) => r.width === fullWidth && r.height === fullHeight,
  );
  let targetWidth: number;
  let targetHeight: number;
  if (baseResIdx >= 0 && baseResIdx < RESOLUTION_TIERS.length - 1) {
    targetWidth = RESOLUTION_TIERS[baseResIdx + 1].width;
    targetHeight = RESOLUTION_TIERS[baseResIdx + 1].height;
  } else if (baseResIdx < 0) {
    targetWidth = 1920;
    targetHeight = 1080;
  } else {
    // Already at the smallest tier
    targetWidth = RESOLUTION_TIERS[RESOLUTION_TIERS.length - 1].width;
    targetHeight = RESOLUTION_TIERS[RESOLUTION_TIERS.length - 1].height;
  }

  const areaScale = (targetWidth * targetHeight) / (fullWidth * fullHeight);
  const fpsScale = reducedFps / fullFps;
  return {
    width: targetWidth,
    height: targetHeight,
    fps: reducedFps,
    bitrate: Math.round(fullBitrate * areaScale * fpsScale),
  };
}

function layerAboveFloor(layer: PublishedLayer, floor: LayerFloor): boolean {
  return (
    layer.width >= floor.minWidth &&
    layer.height >= floor.minHeight &&
    layer.bitrate >= floor.minBitrate &&
    layer.fps >= floor.minFramerate
  );
}

function clampLayerToFloor(layer: PublishedLayer, floor: LayerFloor): PublishedLayer {
  return {
    width: Math.max(layer.width, floor.minWidth),
    height: Math.max(layer.height, floor.minHeight),
    fps: Math.max(layer.fps, floor.minFramerate),
    bitrate: Math.max(layer.bitrate, floor.minBitrate),
  };
}

/* ─── Policy step ────────────────────────────────────────────────── */

export interface PolicyStep {
  sample: NetworkSample;
  stateBefore: Readonly<AdaptationModelState>;
  stateAfter: Readonly<AdaptationModelState>;
  publishedLayer: PublishedLayer;
  telemetryEvents: TelemetryEvent[];
}

function applyStep(
  state: AdaptationModelState,
  sample: NetworkSample,
  profile: ShareProfile,
  ts: number,
): { nextState: AdaptationModelState; layer: PublishedLayer; events: TelemetryEvent[] } {
  const events: TelemetryEvent[] = [];
  const oldTier = state.tier;

  const isBandwidthLimited = sample.bandwidthKbps < (profile.maxBitrate / 1000) * 0.7;
  let newTier = oldTier;

  // Bandwidth-based step-down
  if (isBandwidthLimited && oldTier !== 'reduced-resolution' && oldTier !== 'floor-held') {
    state.consecutiveBandwidthPolls++;
    if (state.consecutiveBandwidthPolls >= ADAPTIVE_BANDWIDTH_STEPDOWN_POLLS) {
      newTier = oldTier === 'full' ? 'reduced-fps' : 'reduced-resolution';
      state.consecutiveBandwidthPolls = 0;
      state.consecutiveLossPolls = 0;
      state.consecutiveRecoveryPolls = 0;
    }
  } else {
    state.consecutiveBandwidthPolls = 0;

    if (oldTier === 'full' && sample.lossPct > ADAPTIVE_LOSS_THRESHOLD_MODERATE) {
      state.consecutiveLossPolls++;
      state.consecutiveRecoveryPolls = 0;
      if (state.consecutiveLossPolls >= ADAPTIVE_STEPDOWN_POLLS) {
        newTier = 'reduced-fps';
        state.consecutiveLossPolls = 0;
      }
    } else if (
      (oldTier === 'reduced-fps' || oldTier === 'floor-held') &&
      sample.lossPct > ADAPTIVE_LOSS_THRESHOLD_SEVERE
    ) {
      state.consecutiveLossPolls++;
      state.consecutiveRecoveryPolls = 0;
      if (state.consecutiveLossPolls >= ADAPTIVE_STEPDOWN_POLLS) {
        newTier = 'reduced-resolution';
        state.consecutiveLossPolls = 0;
      }
    } else if (oldTier !== 'full' && sample.lossPct < ADAPTIVE_RECOVERY_THRESHOLD) {
      state.consecutiveRecoveryPolls++;
      state.consecutiveLossPolls = 0;
      if (state.consecutiveRecoveryPolls >= ADAPTIVE_RECOVERY_POLLS) {
        newTier = oldTier === 'reduced-resolution' ? 'reduced-fps' : 'full';
        state.consecutiveRecoveryPolls = 0;
      }
    } else if (oldTier === 'full') {
      state.consecutiveLossPolls = 0;
    }
  }

  state.tier = newTier;

  // Derive the target layer for the new tier
  let layer = deriveLayer(newTier, profile);

  // Floor enforcement for Detail_Profile (R16.1, R16.2, Property 3)
  const floor = profile.minReadableLayer;
  if (floor !== null && !layerAboveFloor(layer, floor)) {
    // Would go below floor — hold at floor and emit share.floor.hit
    events.push({
      name: 'share.floor.hit',
      profileId: 'detail',
      floor,
      attemptedWidth: layer.width,
      attemptedHeight: layer.height,
      attemptedBitrate: layer.bitrate,
      ts,
    });
    layer = clampLayerToFloor(layer, floor);
    // Record as floor-held tier so downstream recovery logic still works
    state.tier = 'floor-held';
  }

  return { nextState: { ...state }, layer, events };
}

/* ─── Public model function ──────────────────────────────────────── */

/**
 * Runs the adaptation policy model over a stream of `NetworkSample`s.
 * Returns per-step published layers and emitted telemetry events.
 * Used by the Property 3 PBT (`adaptation-policy-model.property.test.ts`).
 */
export function adaptationPolicyModel(params: {
  sampleStream: NetworkSample[];
  profile: ShareProfile;
  windowMs?: number;
}): {
  publishedLayers: PublishedLayer[];
  telemetryEvents: TelemetryEvent[][];
  steps: PolicyStep[];
} {
  const { sampleStream, profile } = params;

  let state: AdaptationModelState = {
    tier: 'full',
    consecutiveLossPolls: 0,
    consecutiveRecoveryPolls: 0,
    consecutiveBandwidthPolls: 0,
  };

  const publishedLayers: PublishedLayer[] = [];
  const telemetryEvents: TelemetryEvent[][] = [];
  const steps: PolicyStep[] = [];

  let ts = 0;
  for (const sample of sampleStream) {
    ts += sample.dtMs;
    const stateBefore: AdaptationModelState = { ...state };
    const { nextState, layer, events } = applyStep(state, sample, profile, ts);
    state = nextState;
    publishedLayers.push(layer);
    telemetryEvents.push(events);
    steps.push({
      sample,
      stateBefore,
      stateAfter: { ...state },
      publishedLayer: layer,
      telemetryEvents: events,
    });
  }

  return { publishedLayers, telemetryEvents, steps };
}
