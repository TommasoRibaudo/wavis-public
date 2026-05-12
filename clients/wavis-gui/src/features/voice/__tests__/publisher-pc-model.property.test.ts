import { describe, expect, it } from 'vitest';
import fc from 'fast-check';

import {
  publisherPcModel,
  type PublisherPcOperation,
} from '../publisher-pc-model';

const publisherPcOperationArb: fc.Arbitrary<PublisherPcOperation> = fc.record({
  op: fc.constantFrom('publish', 'unpublish', 'republish'),
  kind: fc.constantFrom('video', 'audio_mic', 'audio_sys'),
});

describe('publisher-pc-model Property 1', () => {
  it('keeps transceiver count bounded and never leaves stranded senders across churn', () => {
    fc.assert(
      fc.property(
        fc.array(publisherPcOperationArb, { minLength: 20, maxLength: 200 }),
        (operations) => {
          const model = publisherPcModel(operations);

          for (const step of model.steps) {
            expect(step.transceivers.length).toBeLessThanOrEqual(6);
            expect(step.strandedSenders).toBe(0);
          }

          expect(model.transceivers.length).toBeLessThanOrEqual(6);
          expect(model.strandedSenders).toBe(0);
        },
      ),
      { numRuns: 200 },
    );
  });
});
