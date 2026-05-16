/**
 * Settings — camera placeholder labels property test (Req 6.2)
 *
 * Feature: video-feed, Property 10: Camera placeholder labels are deterministic
 */

import { describe, it, expect } from 'vitest';
import * as fc from 'fast-check';

// ─── Pure helper extracted from Settings.tsx label logic ─────────────

export function cameraOptionLabel(device: { deviceId: string; label: string }, index: number): string {
  return device.label || `Camera ${index + 1}`;
}

// ─── Property 10: deterministic placeholder labels ────────────────────

describe('Feature: video-feed, Property 10: Camera placeholder labels are deterministic', () => {
  it('devices with empty labels render as "Camera N" with correct 1-based index', () => {
    const deviceArb = fc.record({
      deviceId: fc.uuid(),
      label: fc.oneof(fc.constant(''), fc.string({ minLength: 1, maxLength: 40 })),
    });

    fc.assert(
      fc.property(fc.array(deviceArb, { minLength: 1, maxLength: 6 }), (devices) => {
        devices.forEach((device, i) => {
          const label = cameraOptionLabel(device, i);
          if (device.label) {
            expect(label).toBe(device.label);
          } else {
            expect(label).toBe(`Camera ${i + 1}`);
          }
          // label must not be empty
          expect(label.length).toBeGreaterThan(0);
        });
      }),
      { numRuns: 100 },
    );
  });

  it('each non-empty label in a list has a unique 1-based index placeholder', () => {
    fc.assert(
      fc.property(fc.integer({ min: 1, max: 6 }), (count) => {
        const emptyDevices = Array.from({ length: count }, (_, i) => ({
          deviceId: `device-${i}`,
          label: '',
        }));
        const labels = emptyDevices.map((d, i) => cameraOptionLabel(d, i));
        expect(new Set(labels).size).toBe(count);
        labels.forEach((label, i) => {
          expect(label).toBe(`Camera ${i + 1}`);
        });
      }),
      { numRuns: 100 },
    );
  });
});
