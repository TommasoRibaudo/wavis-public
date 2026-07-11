import { getNotificationVolume, getSoundVolumes } from '@features/settings/settings-store';

let cachedVolume: number | null = null;
let cachedSoundVolumes: Record<string, number> | null = null;

// !! DO NOT use `new Audio()` / HTMLAudioElement for sound playback !!
//
// WebKitGTK (Tauri on Linux) does not support loading media via the
// tauri:// custom protocol that Tauri uses in production builds.
// `new Audio('/sounds/foo.mp3')` works in dev mode (http://localhost)
// but silently fails in production. The `.catch(() => {})` on play()
// hid this failure for months.
//
// Instead we use the Web Audio API: fetch() + decodeAudioData() +
// AudioBufferSourceNode. fetch() works with tauri:// in both dev and
// production. This is the same API the voice chat system uses.
let audioCtx: AudioContext | null = null;
let gestureListenerActive = false;

// Cache decoded audio buffers so we only fetch + decode each sound once.
const bufferCache: Map<string, AudioBuffer> = new Map();

export function updateCachedNotificationVolume(volume: number): void {
  cachedVolume = volume;
}

export function updateCachedSoundVolumes(volumes: Record<string, number>): void {
  cachedSoundVolumes = { ...volumes };
}

function getAudioContext(): AudioContext {
  if (!audioCtx) {
    audioCtx = new AudioContext();
  }
  return audioCtx;
}

// Register a one-shot click/keydown listener so the AudioContext is resumed
// on the next user gesture if the immediate resume() call fails or is pending.
function registerGestureListenerIfNeeded(ctx: AudioContext): void {
  if (gestureListenerActive || ctx.state !== 'suspended') return;
  gestureListenerActive = true;
  const resume = () => {
    ctx.resume().catch(() => {});
    document.removeEventListener('click', resume);
    document.removeEventListener('keydown', resume);
    gestureListenerActive = false;
  };
  document.addEventListener('click', resume);
  document.addEventListener('keydown', resume);
}

/**
 * Pre-warm the AudioContext so it is ready before the first notification
 * sound is triggered. Call this when media starts connecting to avoid the
 * AudioContext being created lazily inside an async callback where the
 * browser may refuse to resume it without a prior user gesture.
 *
 * Mirrors the pattern in livekit-media.ts ensureAudioContext().
 */
export function prewarmAudioContext(): void {
  if (typeof AudioContext === 'undefined') return;
  const ctx = getAudioContext();
  if (ctx.state !== 'suspended') return;
  // In Tauri (WebView2/WebKit) the autoplay policy is often more relaxed
  // than in a browser tab, so resume() may succeed without a user gesture.
  ctx.resume().catch(() => {});
  // Register a gesture listener as fallback in case resume() is blocked.
  registerGestureListenerIfNeeded(ctx);
}

async function getAudioBuffer(name: string): Promise<AudioBuffer> {
  const cached = bufferCache.get(name);
  if (cached) return cached;

  // fetch() works with both http:// (dev) and tauri:// (production).
  const url = `${window.location.origin}/sounds/${name}.mp3`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch sound "${name}": ${response.status}`);
  }
  const arrayBuffer = await response.arrayBuffer();
  const ctx = getAudioContext();
  const audioBuffer = await ctx.decodeAudioData(arrayBuffer);
  bufferCache.set(name, audioBuffer);
  return audioBuffer;
}

export async function playNotificationSound(name: string): Promise<void> {
  const masterVolume = cachedVolume ?? (await getNotificationVolume());
  if (cachedVolume === null) cachedVolume = masterVolume;
  if (masterVolume === 0) return;

  const soundVolumes = cachedSoundVolumes ?? (await getSoundVolumes());
  if (cachedSoundVolumes === null) cachedSoundVolumes = { ...soundVolumes };

  const individualVolume = soundVolumes[name] ?? 100;
  const effectiveVolume = (masterVolume / 100) * (individualVolume / 100);
  if (effectiveVolume === 0) return;

  try {
    const ctx = getAudioContext();

    // Resume AudioContext if suspended (browser autoplay policy).
    if (ctx.state === 'suspended') {
      registerGestureListenerIfNeeded(ctx);
      await ctx.resume();
    }

    const buffer = await getAudioBuffer(name);

    const gainNode = ctx.createGain();
    gainNode.gain.value = effectiveVolume;
    gainNode.connect(ctx.destination);

    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(gainNode);
    source.start();
  } catch (e) {
    console.error(`[notification-sounds] "${name}" failed:`, e);
  }
}
