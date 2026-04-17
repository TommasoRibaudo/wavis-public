/**
 * Wavis Audio Device Management
 *
 * Delegates to the active media module for device enumeration
 * and switching. Falls back to empty lists when no module is active
 * (graceful degradation). Preserves the AudioDevice interface for
 * backward compatibility with DevicePanel in ActiveRoom.tsx.
 */

import { invoke } from '@tauri-apps/api/core';
import { setStoreValue, STORE_KEYS, inputVolumeToGain } from '@features/settings/settings-store';

const LOG = '[wavis:audio-devices]';

/* ─── Types ─────────────────────────────────────────────────────── */

export interface AudioDevice {
  id: string;
  name: string;
  kind: 'input' | 'output';
  is_default: boolean;
}

/* ─── Module State ──────────────────────────────────────────────── */

interface DeviceCapableMediaModule {
  listDevices(): Promise<{ inputs: MediaDeviceInfo[]; outputs: MediaDeviceInfo[] }>;
  setInputDevice(deviceId: string): Promise<void>;
  setOutputDevice(deviceId: string): Promise<void>;
  setInputVolume(volume: number): Promise<void>;
  setDenoiseEnabled?(enabled: boolean): Promise<void>;
}

let activeMediaModule: DeviceCapableMediaModule | null = null;

/** Called by voice-room.ts connectMedia() to wire the active module. */
export function setActiveLiveKitModule(module: DeviceCapableMediaModule | null): void {
  activeMediaModule = module;
  console.log(LOG, module ? 'module set' : 'module cleared');
}

/* ─── API Functions ─────────────────────────────────────────────── */

export async function listAudioDevices(): Promise<AudioDevice[]> {
  if (!activeMediaModule) return [];

  try {
    const { inputs, outputs } = await activeMediaModule.listDevices();
    const devices: AudioDevice[] = [
      ...inputs.map((d) => ({
        id: d.deviceId,
        name: d.label || `Input ${d.deviceId.slice(0, 8)}`,
        kind: 'input' as const,
        is_default: d.deviceId === 'default',
      })),
      ...outputs.map((d) => ({
        id: d.deviceId,
        name: d.label || `Output ${d.deviceId.slice(0, 8)}`,
        kind: 'output' as const,
        is_default: d.deviceId === 'default',
      })),
    ];
    return devices;
  } catch (err) {
    console.warn(LOG, 'device enumeration failed:', err instanceof Error ? err.message : String(err));
    return [];
  }
}

/** Set the microphone input volume (0–100).
 *  When in a channel, routes through the active media module for an immediate
 *  live change. When not in a channel, falls back to the Tauri command to update
 *  the CPAL gain for the active streams. */
export async function setAudioInputVolume(volume: number): Promise<void> {
  if (activeMediaModule) {
    await activeMediaModule.setInputVolume(volume);
  } else {
    await invoke('set_input_gain', { gain: inputVolumeToGain(volume) });
    await setStoreValue(STORE_KEYS.inputVolume, volume);
  }
}

/** Toggle noise suppression on the active JS LiveKit mic path (Windows/macOS).
 *  No-op when no module is active or the module does not support the method. */
export async function setMediaDenoiseEnabled(enabled: boolean): Promise<void> {
  await activeMediaModule?.setDenoiseEnabled?.(enabled);
}

/** Set the active audio device by ID and kind.
 *  When in a channel, routes through the active media module for an immediate
 *  live switch. When not in a channel, falls back to the Tauri command to prime
 *  the CPAL backend for the next stream start. */
export async function setAudioDevice(deviceId: string, kind: 'input' | 'output'): Promise<void> {
  if (activeMediaModule) {
    if (kind === 'input') {
      await activeMediaModule.setInputDevice(deviceId);
    } else {
      await activeMediaModule.setOutputDevice(deviceId);
    }
  } else {
    // Not in channel — prime the CPAL backend for when streams next start.
    await invoke('set_audio_device', { deviceId, kind });
  }
}
