/**
 * Wavis Screen Share Frame Bridge
 *
 * Bridges the Rust-native canvas-fallback frame delivery path used when a
 * platform can't stream screen-share video as a MediaStream (Linux/WebKitGTK
 * canvas fallback, MJPEG-over-HTTP). Two delivery mechanisms exist side by
 * side: a pushed Tauri event per frame, and a pull-based polling command —
 * callers race both and use whichever arrives.
 *
 * UI components must use this module instead of importing @tauri-apps/*
 * directly (see eslint no-restricted-imports fence).
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

const LOG = '[wavis:screen-share-frame-bridge]';

export interface PolledScreenShareFrame {
  identity: string;
  frame: string;
  width: number;
  height: number;
  seq: number;
}

/** MJPEG-over-HTTP stream URL for a given LiveKit identity, if the platform exposes one. */
export function getScreenShareStreamUrl(identity: string): Promise<string> {
  return invoke<string>('media_get_screen_share_stream_url', { identity });
}

/** Pull the next available frame for identity newer than lastSeq, or null if none. */
export function pollScreenShareFrame(
  identity: string,
  lastSeq: number | null,
): Promise<PolledScreenShareFrame | null> {
  return invoke<PolledScreenShareFrame | null>('media_poll_screen_share_frame', {
    identity,
    lastSeq,
  });
}

interface RawFramePayload {
  identity?: string;
  frame: string;
  width?: number;
  height?: number;
}

/**
 * Subscribe to pushed frame events for a given LiveKit identity. Covers the
 * identity-scoped Linux channel plus the two generic cross-platform channel
 * names, filtering the generic ones by payload.identity when present.
 */
export function onScreenShareFrame(
  identity: string,
  handler: (payload: RawFramePayload) => void,
): () => void {
  const filtered = (payload: RawFramePayload) => {
    if (payload.identity && payload.identity !== identity) return;
    handler(payload);
  };

  let unlistenScoped: (() => void) | null = null;
  let unlistenGeneric: (() => void) | null = null;
  let unlistenGenericWin: (() => void) | null = null;
  let disposed = false;

  listen<RawFramePayload>(`ss-frame:${identity}`, (event) => filtered(event.payload))
    .then((fn) => {
      if (disposed) fn();
      else unlistenScoped = fn;
    })
    .catch((err) => console.error(LOG, `failed to listen for ss-frame:${identity}:`, err));

  listen<RawFramePayload>('screen_share_frame', (event) => filtered(event.payload))
    .then((fn) => {
      if (disposed) fn();
      else unlistenGeneric = fn;
    })
    .catch((err) => console.error(LOG, 'failed to listen for screen_share_frame:', err));

  listen<RawFramePayload>('screen-share-frame', (event) => filtered(event.payload))
    .then((fn) => {
      if (disposed) fn();
      else unlistenGenericWin = fn;
    })
    .catch((err) => console.error(LOG, 'failed to listen for screen-share-frame:', err));

  return () => {
    disposed = true;
    unlistenScoped?.();
    unlistenGeneric?.();
    unlistenGenericWin?.();
  };
}
