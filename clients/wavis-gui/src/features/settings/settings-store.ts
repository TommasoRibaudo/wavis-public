/**
 * Wavis Settings Store
 *
 * Centralized Tauri store wrapper for all user preferences.
 * Provides typed get/set functions with defaults.
 * Separate store file from auth to avoid conflicts.
 */

import { load } from '@tauri-apps/plugin-store';
import { PROFILE_COLORS } from '@shared/colors';

// ─── Types ─────────────────────────────────────────────────────────

export interface ReconnectConfig {
  strategy: 'exponential' | 'fixed';
  baseDelayMs: number;
  maxDelayMs: number;
  maxRetries: number;
}

export interface NotificationToggles {
  participantJoined: boolean;
  participantLeft: boolean;
  participantKicked: boolean;
  participantMutedByHost: boolean;
  inviteReceived: boolean;
}

export interface ChannelVolumePrefs {
  master: number;
  participants: Record<string, number>;
}

// ─── Constants ─────────────────────────────────────────────────────

export const SETTINGS_STORE_PATH = 'wavis-settings.json';

export const STORE_KEYS = {
  profileColor: 'wavis_profile_color',
  tlsEnabled: 'wavis_tls_enabled',
  audioInputDevice: 'wavis_audio_input_device',
  audioOutputDevice: 'wavis_audio_output_device',
  defaultVolume: 'wavis_default_volume',
  notifyParticipantJoined: 'wavis_notify_participant_joined',
  notifyParticipantLeft: 'wavis_notify_participant_left',
  notifyParticipantKicked: 'wavis_notify_participant_kicked',
  notifyParticipantMutedByHost: 'wavis_notify_participant_muted_by_host',
  notifyInviteReceived: 'wavis_notify_invite_received',
  minimizeToTray: 'wavis_minimize_to_tray',
  reconnectConfig: 'wavis_reconnect_config',
  muteHotkey: 'wavis_mute_hotkey',
  watchAllHotkey: 'wavis_watch_all_hotkey',
  logLevel: 'wavis_log_level',
  denoiseEnabled: 'wavis_denoise_enabled',
  channelVolumes: 'wavis_channel_volumes',
  bugReportButtonPos: 'wavis_bug_report_button_pos',
  notificationVolume: 'wavis_notification_volume',
  soundVolumes: 'wavis_notification_sound_volumes',
  inputVolume: 'wavis_input_volume',
} as const;

export const DEFAULT_RECONNECT_CONFIG: ReconnectConfig = {
  strategy: 'exponential',
  baseDelayMs: 1000,
  maxDelayMs: 30000,
  maxRetries: 10,
};

export const DEFAULT_VOLUME = 70;
export const DEFAULT_MUTE_HOTKEY = 'Ctrl+Shift+M';
export const DEFAULT_WATCH_ALL_HOTKEY = 'CmdOrCtrl+Shift+W';

const LOG_PREFIX = '[wavis:settings]';

// ─── Session State ─────────────────────────────────────────────────

let storeInstance: Awaited<ReturnType<typeof load>> | null = null;

// ─── Helpers (private) ─────────────────────────────────────────────

async function getStore(): Promise<Awaited<ReturnType<typeof load>>> {
  if (!storeInstance) {
    storeInstance = await load(SETTINGS_STORE_PATH, { defaults: {}, autoSave: false });
  }
  return storeInstance;
}

// ─── Notification key mapping ──────────────────────────────────────

const NOTIFICATION_KEY_MAP: Record<keyof NotificationToggles, string> = {
  participantJoined: STORE_KEYS.notifyParticipantJoined,
  participantLeft: STORE_KEYS.notifyParticipantLeft,
  participantKicked: STORE_KEYS.notifyParticipantKicked,
  participantMutedByHost: STORE_KEYS.notifyParticipantMutedByHost,
  inviteReceived: STORE_KEYS.notifyInviteReceived,
};

// ─── API Functions (exported) ──────────────────────────────────────

export async function getStoreValue<T>(key: string, defaultValue: T): Promise<T> {
  try {
    const store = await getStore();
    const value = await store.get<T>(key);
    return value ?? defaultValue;
  } catch (err) {
    console.warn(LOG_PREFIX, `Failed to read key "${key}", using default:`, err);
    return defaultValue;
  }
}

export async function setStoreValue<T>(key: string, value: T): Promise<void> {
  try {
    const store = await getStore();
    await store.set(key, value);
    await store.save();
  } catch (err) {
    console.error(LOG_PREFIX, `Failed to persist key "${key}":`, err);
  }
}

// ─── Profile Color ─────────────────────────────────────────────────

export async function getProfileColor(): Promise<string> {
  return getStoreValue(STORE_KEYS.profileColor, PROFILE_COLORS[0]);
}

export async function setProfileColor(color: string): Promise<void> {
  return setStoreValue(STORE_KEYS.profileColor, color);
}

// ─── Default Volume ────────────────────────────────────────────────

export async function getDefaultVolume(): Promise<number> {
  return getStoreValue(STORE_KEYS.defaultVolume, DEFAULT_VOLUME);
}

export async function setDefaultVolume(volume: number): Promise<void> {
  return setStoreValue(STORE_KEYS.defaultVolume, volume);
}

// ─── Notification Toggles ──────────────────────────────────────────

export async function getNotificationToggles(): Promise<NotificationToggles> {
  const [joined, left, kicked, mutedByHost, invite] = await Promise.all([
    getStoreValue(STORE_KEYS.notifyParticipantJoined, true),
    getStoreValue(STORE_KEYS.notifyParticipantLeft, true),
    getStoreValue(STORE_KEYS.notifyParticipantKicked, true),
    getStoreValue(STORE_KEYS.notifyParticipantMutedByHost, true),
    getStoreValue(STORE_KEYS.notifyInviteReceived, true),
  ]);
  return {
    participantJoined: joined,
    participantLeft: left,
    participantKicked: kicked,
    participantMutedByHost: mutedByHost,
    inviteReceived: invite,
  };
}

export async function setNotificationToggle(
  event: keyof NotificationToggles,
  enabled: boolean,
): Promise<void> {
  const key = NOTIFICATION_KEY_MAP[event];
  return setStoreValue(key, enabled);
}

export async function isNotificationEnabled(
  event: keyof NotificationToggles,
): Promise<boolean> {
  const key = NOTIFICATION_KEY_MAP[event];
  return getStoreValue(key, true);
}

// ─── Minimize to Tray ──────────────────────────────────────────────

export async function getMinimizeToTray(): Promise<boolean> {
  return getStoreValue(STORE_KEYS.minimizeToTray, false);
}

export async function setMinimizeToTray(enabled: boolean): Promise<void> {
  return setStoreValue(STORE_KEYS.minimizeToTray, enabled);
}

// ─── Reconnect Config ──────────────────────────────────────────────

export async function getReconnectConfig(): Promise<ReconnectConfig> {
  return getStoreValue(STORE_KEYS.reconnectConfig, DEFAULT_RECONNECT_CONFIG);
}

export async function setReconnectConfig(config: ReconnectConfig): Promise<void> {
  return setStoreValue(STORE_KEYS.reconnectConfig, config);
}

// ─── Mute Hotkey ───────────────────────────────────────────────────

export async function getMuteHotkey(): Promise<string> {
  return getStoreValue(STORE_KEYS.muteHotkey, DEFAULT_MUTE_HOTKEY);
}

export async function setMuteHotkey(hotkey: string): Promise<void> {
  return setStoreValue(STORE_KEYS.muteHotkey, hotkey);
}

// ─── Watch All Hotkey ──────────────────────────────────────────────

export async function getWatchAllHotkey(): Promise<string> {
  return getStoreValue(STORE_KEYS.watchAllHotkey, DEFAULT_WATCH_ALL_HOTKEY);
}

export async function setWatchAllHotkey(hotkey: string): Promise<void> {
  return setStoreValue(STORE_KEYS.watchAllHotkey, hotkey);
}

// ─── Denoise ───────────────────────────────────────────────────────

export async function getDenoiseEnabled(): Promise<boolean> {
  return getStoreValue(STORE_KEYS.denoiseEnabled, true);
}

export async function setDenoiseEnabled(enabled: boolean): Promise<void> {
  return setStoreValue(STORE_KEYS.denoiseEnabled, enabled);
}

// ─── Input Volume ──────────────────────────────────────────────────

/** Returns the saved microphone input volume (0–100), defaulting to 100 (unity gain). */
export async function getInputVolume(): Promise<number> {
  return getStoreValue(STORE_KEYS.inputVolume, 100);
}

export async function setInputVolume(volume: number): Promise<void> {
  return setStoreValue(STORE_KEYS.inputVolume, volume);
}

/**
 * Maps a 0–100 slider value to a 0–1 linear gain using a square law so that
 * 50% on the slider produces approximately half the perceived loudness (−12 dB).
 * Matches the perceptual curve pattern used for output volumes.
 */
export function inputVolumeToGain(volume: number): number {
  const v = Math.max(0, Math.min(100, volume)) / 100;
  return v * v;
}

// ─── Audio Output Device ───────────────────────────────────────────

/** Returns the saved audio output device ID, or null if none has been selected. */
export async function getAudioOutputDevice(): Promise<string | null> {
  return getStoreValue<string | null>(STORE_KEYS.audioOutputDevice, null);
}

// ─── Audio Input Device ────────────────────────────────────────────

/** Returns the saved audio input device ID, or null if none has been selected. */
export async function getAudioInputDevice(): Promise<string | null> {
  return getStoreValue<string | null>(STORE_KEYS.audioInputDevice, null);
}

// ─── Notification Volume ───────────────────────────────────────────

export async function getNotificationVolume(): Promise<number> {
  return getStoreValue(STORE_KEYS.notificationVolume, 50);
}

export async function setNotificationVolume(volume: number): Promise<void> {
  return setStoreValue(STORE_KEYS.notificationVolume, volume);
}

// ─── Per-Sound Notification Volumes ────────────────────────────────

export async function getSoundVolumes(): Promise<Record<string, number>> {
  return getStoreValue(STORE_KEYS.soundVolumes, {});
}

export async function setSoundVolumes(volumes: Record<string, number>): Promise<void> {
  return setStoreValue(STORE_KEYS.soundVolumes, volumes);
}

// ─── Channel Volume Preferences ────────────────────────────────────

export async function getChannelVolumes(channelId: string): Promise<ChannelVolumePrefs | null> {
  const all = await getStoreValue<Record<string, ChannelVolumePrefs>>(STORE_KEYS.channelVolumes, {});
  return all[channelId] ?? null;
}

export async function setChannelVolumes(channelId: string, prefs: ChannelVolumePrefs): Promise<void> {
  const all = await getStoreValue<Record<string, ChannelVolumePrefs>>(STORE_KEYS.channelVolumes, {});
  all[channelId] = prefs;
  return setStoreValue(STORE_KEYS.channelVolumes, all);
}
