import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Mirrors the constants in ../wasapi-audio-worklet.js — kept in sync manually
// since the worklet is loaded as a raw AudioWorklet module, not a normal import.
const RING_SIZE = 48000 * 0.2; // 9600
const PRIME_SAMPLES = 48000 * 0.08; // 3840

type StatsPayload = {
  underruns: number;
  reprimes: number;
  overflowSheds: number;
  framesQueued: number;
  occupancy: number;
};

type WorkletInternals = {
  _count: number;
  _overflowSheds: number;
  _underruns: number;
  _reprimes: number;
};

type WorkletInstance = WorkletInternals & {
  port: {
    onmessage: ((e: { data: unknown }) => void) | null;
    postMessage: ReturnType<typeof vi.fn>;
  };
  process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean;
};

let CapturedProcessor: (new () => WorkletInstance) | undefined;

beforeEach(() => {
  vi.resetModules();
  CapturedProcessor = undefined;

  class AudioWorkletProcessorStub {
    port: {
      onmessage: ((e: { data: unknown }) => void) | null;
      postMessage: ReturnType<typeof vi.fn>;
    };
    constructor() {
      this.port = { onmessage: null, postMessage: vi.fn() };
    }
  }
  vi.stubGlobal('AudioWorkletProcessor', AudioWorkletProcessorStub);
  vi.stubGlobal('registerProcessor', (_name: string, cls: new () => WorkletInstance) => {
    CapturedProcessor = cls;
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

async function loadProcessor(): Promise<WorkletInstance> {
  // @ts-expect-error — plain JS AudioWorklet module, no type declarations
  await import('../wasapi-audio-worklet.js');
  if (!CapturedProcessor) throw new Error('registerProcessor was not called');
  return new CapturedProcessor();
}

function rampFrame(len: number, start: number): Float32Array {
  const f = new Float32Array(len);
  for (let i = 0; i < len; i++) f[i] = start + i;
  return f;
}

function sendFrame(node: WorkletInstance, samples: Float32Array): void {
  node.port.onmessage?.({ data: samples });
}

function runQuantum(node: WorkletInstance, len = 128): Float32Array {
  const channel = new Float32Array(len);
  node.process([], [[channel]]);
  return channel;
}

describe('wasapi-audio-worklet', () => {
  it('reproduces the bug scenario gap-free after priming under simulated delivery jitter', async () => {
    const node = await loadProcessor();

    // Prime with a continuous ramp so any dropped/zeroed/reordered sample
    // breaks the "always +1" invariant checked below.
    sendFrame(node, rampFrame(PRIME_SAMPLES, 0));
    let next = PRIME_SAMPLES;

    const collected: number[] = [];
    // 40 deliveries of a 20ms/960-sample frame (steady state = 7.5 quanta/frame),
    // with occasional jitter: an extra quantum consumed before the next frame
    // arrives, simulating main-thread jank / IPC delay in the real pipeline.
    for (let delivery = 0; delivery < 40; delivery++) {
      const quantaThisDelivery = delivery % 5 === 0 ? 8 : 7; // alternates around 7.5 avg, +1 on jitter beats
      for (let q = 0; q < quantaThisDelivery; q++) {
        collected.push(...runQuantum(node));
      }
      sendFrame(node, rampFrame(960, next));
      next += 960;
    }
    // Drain remaining buffered samples.
    for (let q = 0; q < 40; q++) collected.push(...runQuantum(node));

    expect(node._underruns).toBe(0);
    for (let i = 1; i < collected.length; i++) {
      expect(collected[i]).toBe(collected[i - 1] + 1);
    }
  });

  it('stays silent until PRIME_SAMPLES are buffered, without dropping queued samples', async () => {
    const node = await loadProcessor();

    sendFrame(node, rampFrame(PRIME_SAMPLES - 1, 0));
    const early = runQuantum(node);
    expect(Array.from(early)).toEqual(new Array<number>(128).fill(0));

    sendFrame(node, rampFrame(1, PRIME_SAMPLES - 1)); // now exactly PRIME_SAMPLES queued
    const afterPrime = runQuantum(node);
    expect(Array.from(afterPrime)).toEqual(Array.from({ length: 128 }, (_, i) => i));
  });

  it('re-primes after a mid-quantum underrun and resumes with no lost samples', async () => {
    const node = await loadProcessor();

    sendFrame(node, rampFrame(PRIME_SAMPLES, 0));
    sendFrame(node, rampFrame(1, PRIME_SAMPLES)); // one extra sample, value 3840

    // Drain 30 full quanta (3840 samples), leaving exactly 1 buffered.
    for (let i = 0; i < 30; i++) runQuantum(node);

    const underrunQuantum = runQuantum(node);
    expect(underrunQuantum[0]).toBe(PRIME_SAMPLES); // last buffered sample
    expect(Array.from(underrunQuantum.slice(1))).toEqual(new Array<number>(127).fill(0));
    expect(node._underruns).toBe(1);
    expect(node._reprimes).toBe(1);

    // Still silent while empty and not yet re-primed.
    const stillSilent = runQuantum(node);
    expect(Array.from(stillSilent)).toEqual(new Array<number>(128).fill(0));
    expect(node._underruns).toBe(1); // no new underrun while idle-empty

    sendFrame(node, rampFrame(PRIME_SAMPLES, 5000));
    const resumed = runQuantum(node);
    expect(Array.from(resumed)).toEqual(Array.from({ length: 128 }, (_, i) => 5000 + i));
    expect(node._reprimes).toBe(1);
  });

  it('sheds overflow in one bounded chunk and stays continuous afterwards', async () => {
    const node = await loadProcessor();

    sendFrame(node, rampFrame(RING_SIZE, 0)); // fills ring exactly
    expect(node._count).toBe(RING_SIZE);

    sendFrame(node, rampFrame(100, 20000)); // triggers overflow

    expect(node._overflowSheds).toBe(1);
    expect(node._count).toBe(PRIME_SAMPLES + 100);

    // Continuity check: the retained tail of the first ramp should be
    // immediately readable (no silence gap from the shed).
    const quantum = runQuantum(node);
    const expectedStart = RING_SIZE - PRIME_SAMPLES; // 5760
    expect(Array.from(quantum)).toEqual(Array.from({ length: 128 }, (_, i) => expectedStart + i));
  });

  it('stops processing on the poison pill', async () => {
    const node = await loadProcessor();

    sendFrame(node, rampFrame(PRIME_SAMPLES, 0));
    sendFrame(node, null as unknown as Float32Array);

    const channel = new Float32Array(128);
    const keepProcessing = node.process([], [[channel]]);
    expect(keepProcessing).toBe(false);
  });

  it('emits stats via postMessage', async () => {
    const node = await loadProcessor();

    sendFrame(node, rampFrame(PRIME_SAMPLES, 0));
    for (let i = 0; i < 1875; i++) runQuantum(node);

    expect(node.port.postMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        type: 'stats',
        payload: expect.objectContaining<Partial<StatsPayload>>({
          underruns: expect.any(Number) as number,
          reprimes: expect.any(Number) as number,
          overflowSheds: expect.any(Number) as number,
          framesQueued: expect.any(Number) as number,
          occupancy: expect.any(Number) as number,
        }),
      }),
    );
  });
});
