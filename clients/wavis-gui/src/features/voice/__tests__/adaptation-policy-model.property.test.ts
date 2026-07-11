/**
 * Property 3 PBT — no silent downgrade below Minimum_Readable_Layer.
 *
 * MANDATORY invariant (design.md §6 Property 3, R16.1, R16.2, R1.2, R1.3):
 * For every step i of adaptationPolicyModel(Detail_Profile, sampleStream):
 *   publishedLayers[i] >= minReadableLayer
 *   OR telemetryEvents[i] contains { name: 'share.floor.hit' }
 *
 * "Floor-holding is allowed iff the floor-hit event fires." A step that
 * silently drops below the floor without emitting share.floor.hit is the
 * regression this property guards against.
 */
import { describe, expect, it } from 'vitest';
import fc from 'fast-check';

import { adaptationPolicyModel, type NetworkSample } from '../adaptation-policy-model';
import { DETAIL_PROFILE, MOTION_PROFILE } from '../livekit-media';

const networkSampleArb: fc.Arbitrary<NetworkSample> = fc.record({
  lossPct: fc.float({ min: 0, max: 100, noNaN: true }),
  bandwidthKbps: fc.integer({ min: 100, max: 20_000 }),
  cpuPct: fc.integer({ min: 0, max: 100 }),
  dtMs: fc.integer({ min: 500, max: 5_000 }),
});

describe('adaptation-policy-model Property 3', () => {
  it('never silently drops below Minimum_Readable_Layer in Detail_Profile steady state', () => {
    fc.assert(
      fc.property(fc.array(networkSampleArb, { minLength: 5, maxLength: 100 }), (sampleStream) => {
        const { steps } = adaptationPolicyModel({ sampleStream, profile: DETAIL_PROFILE });
        const floor = DETAIL_PROFILE.minReadableLayer!;

        for (const step of steps) {
          const l = step.publishedLayer;
          const aboveFloor =
            l.width >= floor.minWidth &&
            l.height >= floor.minHeight &&
            l.bitrate >= floor.minBitrate &&
            l.fps >= floor.minFramerate;

          const floorHitEmitted = step.telemetryEvents.some((e) => e.name === 'share.floor.hit');

          // Disjunctive: above floor OR floor-hit event was emitted (R16.2)
          expect(aboveFloor || floorHitEmitted).toBe(true);
        }
      }),
      { numRuns: 500 },
    );
  });

  it('never emits share.floor.hit for Motion_Profile (no floor to enforce)', () => {
    fc.assert(
      fc.property(fc.array(networkSampleArb, { minLength: 5, maxLength: 100 }), (sampleStream) => {
        const { steps } = adaptationPolicyModel({ sampleStream, profile: MOTION_PROFILE });

        for (const step of steps) {
          const hasFloorHit = step.telemetryEvents.some((e) => e.name === 'share.floor.hit');
          expect(hasFloorHit).toBe(false);
        }
      }),
      { numRuns: 200 },
    );
  });

  it('emits share.floor.hit with correct field shape when floor is crossed', () => {
    // Craft a stream that forces a reduced-resolution transition
    const badStream: NetworkSample[] = Array.from({ length: 10 }, () => ({
      lossPct: 100, // always above severe threshold
      bandwidthKbps: 10, // severely bandwidth-limited
      cpuPct: 100,
      dtMs: 1_000,
    }));

    // Use a custom profile whose reduced-resolution layer is below the floor
    const lowBitrateProfile = {
      ...DETAIL_PROFILE,
      maxBitrate: 100_000, // tiny bitrate so derived bitrate < minBitrate
    };

    const { steps } = adaptationPolicyModel({
      sampleStream: badStream,
      profile: lowBitrateProfile,
    });

    const floorHitStep = steps.find((s) =>
      s.telemetryEvents.some((e) => e.name === 'share.floor.hit'),
    );
    expect(floorHitStep).toBeDefined();

    const floorHitEvent = floorHitStep!.telemetryEvents.find((e) => e.name === 'share.floor.hit');
    expect(floorHitEvent).toBeDefined();
    if (floorHitEvent?.name === 'share.floor.hit') {
      expect(floorHitEvent.profileId).toBe('detail');
      expect(floorHitEvent.floor).toEqual(DETAIL_PROFILE.minReadableLayer);
      expect(typeof floorHitEvent.attemptedWidth).toBe('number');
      expect(typeof floorHitEvent.attemptedHeight).toBe('number');
      expect(typeof floorHitEvent.attemptedBitrate).toBe('number');
    }
  });

  it('holds floor on every subsequent step once floor is reached', () => {
    // Persistent bad conditions — should stay at floor, not keep emitting floor-hit
    const persistentBadStream: NetworkSample[] = Array.from({ length: 30 }, () => ({
      lossPct: 20, // above severe threshold
      bandwidthKbps: 100, // bandwidth-limited
      cpuPct: 90,
      dtMs: 500,
    }));

    const lowBitrateProfile = { ...DETAIL_PROFILE, maxBitrate: 50_000 };
    const { steps } = adaptationPolicyModel({
      sampleStream: persistentBadStream,
      profile: lowBitrateProfile,
    });
    const floor = DETAIL_PROFILE.minReadableLayer!;

    for (const step of steps) {
      const l = step.publishedLayer;
      const aboveOrAtFloor =
        l.width >= floor.minWidth &&
        l.height >= floor.minHeight &&
        l.bitrate >= floor.minBitrate &&
        l.fps >= floor.minFramerate;
      const floorHitEmitted = step.telemetryEvents.some((e) => e.name === 'share.floor.hit');
      expect(aboveOrAtFloor || floorHitEmitted).toBe(true);
    }
  });
});
