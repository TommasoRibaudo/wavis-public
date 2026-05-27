import { describe, expect, it } from 'vitest';
import * as fc from 'fast-check';

import { cameraButtonLabel, shouldMountCameraButton } from '../active-room-camera';

  describe('Feature: video-feed, Property 4: Button label depends only on intent', () => {
    it('returns camera-off iff camera intent is true, regardless of publication state', () => {
      const publicationStateArb = fc.constantFrom<'idle' | 'opening' | 'publishing' | 'published' | 'failing'>(
      'idle',
      'opening',
      'publishing',
      'published',
      'failing',
    );

    fc.assert(
        fc.property(fc.boolean(), publicationStateArb, (cameraIntent, cameraPublication) => {
          expect(cameraButtonLabel(cameraIntent)).toBe(cameraIntent ? 'camera-off' : 'camera-on');

          void cameraPublication;
          expect(cameraButtonLabel(cameraIntent)).toBe(cameraIntent ? 'camera-off' : 'camera-on');
        }),
      { numRuns: 100 },
    );
  });
});

describe('Feature: video-feed, Property 7: Mount gate', () => {
  it('mounts the camera button iff the voice room is connected on a supported capture platform', () => {
    fc.assert(
      fc.property(fc.boolean(), fc.boolean(), (voiceRoomConnected, supportedCapturePlatform) => {
        expect(
          shouldMountCameraButton(voiceRoomConnected, supportedCapturePlatform),
        ).toBe(voiceRoomConnected && supportedCapturePlatform);
      }),
      { numRuns: 100 },
    );
  });
});
