/**
 * Minimal ambient types for the WebCodecs MediaStreamTrackGenerator API
 * (Chromium 94+, part of the Media Capture Transform spec). Not yet in
 * TypeScript's lib.dom.d.ts and no @types package provides it. Scoped to
 * exactly what livekit-media.ts's native screen-share capture path uses.
 */
interface MediaStreamTrackGeneratorInit {
  kind: 'video' | 'audio';
}

declare class MediaStreamTrackGenerator extends MediaStreamTrack {
  constructor(init: MediaStreamTrackGeneratorInit);
  readonly writable: WritableStream<VideoFrame>;
}
