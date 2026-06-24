import { describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

import { exportSnapshot, type DiagnosticsSnapshot } from '../diagnostics';

function baseSnapshot(): DiagnosticsSnapshot {
  return {
    timestamp: Date.now(),
    appDimensions: null,
    rss: null,
    jsHeap: null,
    domNodes: 0,
    cpuPercent: null,
    network: null,
    audio: null,
    share: null,
    shareActive: false,
    shareMode: null,
    shareSourceName: null,
    shareStartedAt: null,
    shareStoppedAt: null,
    videoReceive: null,
  };
}

describe('diagnostics export', () => {
  it('includes native bridge cadence fields even when sender stats are zeroed', () => {
    const text = exportSnapshot({
      ...baseSnapshot(),
      shareActive: true,
      share: {
        bitrateKbps: 0,
        fps: 0,
        qualityLimitationReason: 'none',
        packetLossPercent: 0,
        frameWidth: 0,
        frameHeight: 0,
        pliCount: 0,
        nackCount: 0,
        availableBandwidthKbps: 0,
        nativeBridge: {
          rustTargetFps: 60,
          jsBridgeFps: 60,
          pollTicks: 12,
          pollHits: 10,
          pollSkippedForWork: 3,
          streamReads: 4,
          streamBytes: 12_582_912,
          streamBytesPerSec: 6_291_456,
          streamReadAvgMs: 1.1,
          writerBackpressureSkips: 2,
          staleFrameDrops: 6,
          writerFps: 58.5,
          duplicateSeqSkips: 8,
          newSeqCount: 2,
          jsDecodedFrames: 7,
          generatorWrites: 2,
          canvasPaints: 0,
          keepaliveWrites: 5,
          decodeFailures: 1,
          avgDecodeMs: 3.4,
          avgWriteMs: 1.2,
          jsObservedRustSeqFps: 12.5,
          duplicatePollRatio: 0.8,
          keepaliveFps: 4.5,
          latestSeqAgeMs: 123,
          realDecodeAvgMs: 3.4,
          realWriteAvgMs: 0.8,
          keepaliveWriteAvgMs: 1.4,
          base64FetchAvgMs: 0.4,
          jpegDecodeAvgMs: 2.7,
          videoFrameCreateAvgMs: 0.3,
          writerAvgMs: 1.2,
          rawI420Frames: 5,
          jpegFallbackFrames: 2,
          pollSkippedForWorkFps: 1.5,
          trackReadyState: 'live',
          trackMuted: false,
        },
      },
    });

    expect(text).toContain('Bridge Target:');
    expect(text).toContain('60 writer target / 60 backend target');
    expect(text).toContain('2 new, 8 dup skips');
    expect(text).toContain('12.5 fps, 80.0% duplicate polls');
    expect(text).toContain('4 reads, 6.0 MiB/s, avg 1.1 ms');
    expect(text).toContain('58.5 fps');
    expect(text).toContain('2 real, 5 keepalive');
    expect(text).toContain('4.5 fps, avg 1.4 ms');
    expect(text).toContain('7 ok, 1 failed, avg 3.4 ms (5 raw I420, 2 JPEG fallback)');
    expect(text).toContain('base64 0.4 ms, jpeg 2.7 ms, vf 0.3 ms');
    expect(text).toContain('3 poll skips, 2 writer skips, 6 stale drops');
    expect(text).toContain('live, muted=false');
  });
});
