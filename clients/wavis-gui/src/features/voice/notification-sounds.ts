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
  const masterVolume = cachedVolume ?? await getNotificationVolume();
  if (cachedVolume === null) cachedVolume = masterVolume;
  if (masterVolume === 0) return;

  const soundVolumes = cachedSoundVolumes ?? await getSoundVolumes();
  if (cachedSoundVolumes === null) cachedSoundVolumes = { ...soundVolumes };

  const individualVolume = soundVolumes[name] ?? 100;
  const effectiveVolume = (masterVolume / 100) * (individualVolume / 100);
  if (effectiveVolume === 0) return;

  try {
    const ctx = getAudioContext();

    // Resume AudioContext if suspended (browser autoplay policy).
    if (ctx.state === 'suspended') {
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
