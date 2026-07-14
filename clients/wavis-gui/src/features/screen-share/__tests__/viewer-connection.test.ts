import { describe, it, expect } from 'vitest';
import { Track } from 'livekit-client';
import {
  computeViewerBackoffMs,
  makeViewerWindowId,
  pickScreenSharePublication,
  type ScreenSharePublicationLike,
} from '../viewer-connection';

describe('computeViewerBackoffMs', () => {
  it('starts at 1s and doubles per attempt', () => {
    expect(computeViewerBackoffMs(0)).toBe(1000);
    expect(computeViewerBackoffMs(1)).toBe(2000);
    expect(computeViewerBackoffMs(2)).toBe(4000);
    expect(computeViewerBackoffMs(3)).toBe(8000);
  });

  it('caps at 15s and never decreases', () => {
    let prev = 0;
    for (let attempt = 0; attempt < 40; attempt++) {
      const delay = computeViewerBackoffMs(attempt);
      expect(delay).toBeGreaterThanOrEqual(prev);
      expect(delay).toBeLessThanOrEqual(15000);
      prev = delay;
    }
    expect(computeViewerBackoffMs(10)).toBe(15000);
  });

  it('clamps negative attempts to the base delay', () => {
    expect(computeViewerBackoffMs(-1)).toBe(1000);
  });
});

describe('makeViewerWindowId', () => {
  it('produces ids within the backend identifier charset and length bound', () => {
    for (let i = 0; i < 50; i++) {
      const id = makeViewerWindowId();
      expect(id).toMatch(/^w[0-9a-f]{8}$/);
      expect(id.length).toBeLessThanOrEqual(32);
    }
  });

  it('produces distinct ids across instances', () => {
    const ids = new Set(Array.from({ length: 100 }, () => makeViewerWindowId()));
    expect(ids.size).toBeGreaterThan(90); // 2^32 space — collisions in 100 draws are a bug
  });
});

describe('pickScreenSharePublication', () => {
  function participantWith(pubs: Partial<Record<Track.Source, ScreenSharePublicationLike>>) {
    return {
      getTrackPublication: (source: Track.Source) => pubs[source],
    };
  }

  it('returns the screen-share video publication', () => {
    const sharePub = { kind: Track.Kind.Video };
    const participant = participantWith({
      [Track.Source.ScreenShare]: sharePub,
      [Track.Source.Camera]: { kind: Track.Kind.Video },
    });
    expect(pickScreenSharePublication(participant)).toBe(sharePub);
  });

  it('ignores a screen-share AUDIO publication surfaced under the video source', () => {
    const participant = participantWith({
      [Track.Source.ScreenShare]: { kind: Track.Kind.Audio },
    });
    expect(pickScreenSharePublication(participant)).toBeNull();
  });

  it('returns null when the participant has no screen-share publication', () => {
    const participant = participantWith({
      [Track.Source.Camera]: { kind: Track.Kind.Video },
    });
    expect(pickScreenSharePublication(participant)).toBeNull();
  });
});
