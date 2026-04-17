import { useState, useEffect, useCallback, lazy, Suspense } from 'react';

const ChannelDetail = lazy(() => import('@features/channels/ChannelDetail'));
import { useNavigate } from 'react-router';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { getVersion, getTauriVersion } from '@tauri-apps/api/app';
import { resetAuth, logout, getServerUrl, getDeviceId, getDisplayName, getAccessToken, INSECURE_TLS_ALLOWED } from '@features/auth/auth';
import { PROFILE_COLORS } from '@shared/colors';
import { getProfileColor, setProfileColor, getStoreValue, setStoreValue, STORE_KEYS, getDefaultVolume, DEFAULT_VOLUME, getMinimizeToTray, setMinimizeToTray, getNotificationToggles, setNotificationToggle, getMuteHotkey, setMuteHotkey, DEFAULT_MUTE_HOTKEY, getWatchAllHotkey, setWatchAllHotkey, DEFAULT_WATCH_ALL_HOTKEY, getDenoiseEnabled, setDenoiseEnabled, getNotificationVolume, setNotificationVolume, getSoundVolumes, setSoundVolumes, getInputVolume, setInputVolume } from './settings-store';
import { updateCachedNotificationVolume, updateCachedSoundVolumes } from '@features/voice/notification-sounds';
import { updateSessionProfileColor, getState as getVoiceRoomState } from '@features/voice/voice-room';
import { VolumeSlider } from '@shared/VolumeSlider';
import { setAudioDevice, setAudioInputVolume, setMediaDenoiseEnabled } from '@features/voice/audio-devices';
import type { NotificationToggles } from './settings-store';
import { redactToken } from '@shared/helpers';
import { useDebug } from '@shared/debug-context';
import { formatHotkeyCombination, unregisterMuteHotkey, unregisterWatchAllHotkey, isHotkeyRegistered } from '@shared/hotkey-bridge';
import { Switch } from '../../components/ui/switch';
import { open } from '@tauri-apps/plugin-shell';
import { ConfirmTextGate } from '@shared/ConfirmTextGate';

/* ─── Audio Types ───────────────────────────────────────────────── */
interface AudioDevice {
  id: string;
  name: string;
  is_default: boolean;
  kind: 'input' | 'output';
}

/** Format device name, appending "(default)" for system default devices */
export function formatDeviceName(device: AudioDevice): string {
  return device.is_default ? `${device.name} (default)` : device.name;
}

type DenoiseStatus = {
  tone: 'active' | 'saved' | 'degraded' | 'disabled';
  message: string;
};

export function describeDenoiseStatus(params: {
  denoiseEnabled: boolean;
  connectionMode: 'livekit' | 'native' | undefined;
  mediaState: 'disconnected' | 'connecting' | 'connected' | 'failed';
  userAgent: string;
  noiseSuppressionActive?: boolean;
}): DenoiseStatus {
  const { denoiseEnabled, connectionMode, mediaState, userAgent, noiseSuppressionActive } = params;
  const isMacOrWindows = /Macintosh|Windows/i.test(userAgent);
  const sessionActive = mediaState === 'connected' || mediaState === 'connecting';

  if (!denoiseEnabled) {
    return {
      tone: 'disabled',
      message: 'Off. When enabled, RNNoise is applied on native Rust audio paths.',
    };
  }

  if (sessionActive && connectionMode === 'native') {
    return {
      tone: 'active',
      message: 'Active on this session. Your microphone is using the native Rust audio path.',
    };
  }

  if (sessionActive && connectionMode === 'livekit' && noiseSuppressionActive) {
    return {
      tone: 'active',
      message: 'Active on this session. Your microphone is using the Wavis JS noise suppression processor.',
    };
  }

  if (sessionActive && connectionMode === 'livekit') {
    return {
      tone: 'degraded',
      message: 'Saved, but this session uses the JS LiveKit mic path where RNNoise is not active.',
    };
  }

  if (isMacOrWindows) {
    return {
      tone: 'saved',
      message: 'Saved. Will apply on next session.',
    };
  }

  return {
    tone: 'saved',
    message: 'Saved. RNNoise applies on native Rust audio sessions; JS fallback sessions may not use it.',
  };
}

/* ─── Constants ─────────────────────────────────────────────────── */
const DIVIDER = '─'.repeat(48);

const SOUND_LABELS: { key: string; label: string }[] = [
  { key: 'join',          label: 'Participant joined' },
  { key: 'leave',         label: 'Participant left' },
  { key: 'share-start',   label: 'Screen share started' },
  { key: 'share-stop',    label: 'Screen share stopped' },
  { key: 'viewer-joined', label: 'Someone joined your stream' },
  { key: 'mute',          label: 'You muted' },
  { key: 'unmute',        label: 'You unmuted' },
  { key: 'deafen',        label: 'You deafened' },
  { key: 'undeafen',      label: 'You undeafened' },
];

/* ═══ Component ═════════════════════════════════════════════════════ */
interface SettingsProps { onClose?: () => void; onNavigateAway?: (path: string) => void; channelId?: string }
export default function Settings({ onClose, onNavigateAway, channelId }: SettingsProps = {}) {
  const navigate = useNavigate();
  const [activeTab, setActiveTab] = useState<'general' | 'channel'>('general');

  const handleNavigateAway = (path: string) => {
    if (onNavigateAway) onNavigateAway(path);
    else navigate(path);
  };
  const { showSecrets } = useDebug();

  const [showConfirm, setShowConfirm] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);
  const [serverUrl, setServerUrl] = useState<string | null>(null);
  const [deviceId, setDeviceId] = useState<string | null>(null);
  const [displayName, setDisplayNameVal] = useState<string | null>(null);
  const [selectedColor, setSelectedColor] = useState<string>(PROFILE_COLORS[0]);
  const [tlsEnabled, setTlsEnabled] = useState(true);
  const [audioDevices, setAudioDevices] = useState<AudioDevice[]>([]);
  const [audioError, setAudioError] = useState<string | null>(null);
  const [selectedInputDevice, setSelectedInputDevice] = useState<string>('');
  const [selectedOutputDevice, setSelectedOutputDevice] = useState<string>('');
  const [volume, setVolume] = useState<number>(DEFAULT_VOLUME);
  const [accessToken, setAccessTokenVal] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState<string>('—');
  const [tauriVersion, setTauriVersion] = useState<string>('—');
  const [osPlatform, setOsPlatform] = useState<string>('—');
  const [webviewVersion, setWebviewVersion] = useState<string>('—');
  const [minimizeToTray, setMinimizeToTrayState] = useState(false);
  const [notifyToggles, setNotifyToggles] = useState<NotificationToggles>({
    participantJoined: true,
    participantLeft: true,
    participantKicked: true,
    participantMutedByHost: true,
    inviteReceived: true,
  });
  const [muteHotkey, setMuteHotkeyState] = useState<string>(DEFAULT_MUTE_HOTKEY);
  const [watchAllHotkey, setWatchAllHotkeyState] = useState<string>(DEFAULT_WATCH_ALL_HOTKEY);
  const [denoiseEnabled, setDenoiseEnabledState] = useState(true);
  const [inputVolume, setInputVolumeState] = useState<number>(100);
  const [notificationVolume, setNotificationVolumeState] = useState<number>(50);
  const [soundVolumes, setSoundVolumesState] = useState<Record<string, number>>({});
  const [showSoundVolumes, setShowSoundVolumes] = useState(false);
  const [recordingHotkey, setRecordingHotkey] = useState(false);
  const [recordedModifiers, setRecordedModifiers] = useState<string[]>([]);
  const [_recordedKey, setRecordedKey] = useState<string | null>(null);
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const [recordingWatchAllHotkey, setRecordingWatchAllHotkey] = useState(false);
  const [recordedWatchAllModifiers, setRecordedWatchAllModifiers] = useState<string[]>([]);
  const [_recordedWatchAllKey, setRecordedWatchAllKey] = useState<string | null>(null);
  const [watchAllHotkeyError, setWatchAllHotkeyError] = useState<string | null>(null);
  const denoiseStatus = describeDenoiseStatus({
    denoiseEnabled,
    connectionMode: getVoiceRoomState().connectionMode,
    mediaState: getVoiceRoomState().mediaState,
    userAgent: navigator.userAgent,
    noiseSuppressionActive: getVoiceRoomState().noiseSuppressionActive,
  });
  useEffect(() => {
    getServerUrl().then(setServerUrl);
    getDeviceId().then(setDeviceId);
    getDisplayName().then(setDisplayNameVal);
    getProfileColor().then(setSelectedColor);
    getStoreValue(STORE_KEYS.tlsEnabled, true).then(setTlsEnabled);
    getDefaultVolume().then(setVolume);
    getStoreValue<string>(STORE_KEYS.audioInputDevice, '').then(setSelectedInputDevice);
    getStoreValue<string>(STORE_KEYS.audioOutputDevice, '').then(setSelectedOutputDevice);
    getAccessToken().then(setAccessTokenVal);
    getMinimizeToTray().then(setMinimizeToTrayState);
    getNotificationToggles().then(setNotifyToggles);
    getMuteHotkey().then(setMuteHotkeyState);
    getWatchAllHotkey().then(setWatchAllHotkeyState);
    getDenoiseEnabled().then(setDenoiseEnabledState);
    getInputVolume().then(setInputVolumeState);
    getNotificationVolume().then(setNotificationVolumeState);
    getSoundVolumes().then(setSoundVolumesState);
    getVersion().then(setAppVersion).catch(() => {});
    getTauriVersion().then(setTauriVersion).catch(() => {});
    setOsPlatform(navigator.platform || 'unknown');
    const ua = navigator.userAgent;
    const webkitMatch = ua.match(/AppleWebKit\/([\d.]+)/);
    const chromeMatch = ua.match(/Chrome\/([\d.]+)/);
    setWebviewVersion(chromeMatch?.[1] ?? webkitMatch?.[1] ?? 'unknown');
    invoke<AudioDevice[]>('list_audio_devices')
      .then(setAudioDevices)
      .catch(() => setAudioError('Failed to load audio devices'));
  }, []);

  // Startup sync: emit minimize-to-tray-changed to Rust after store loads
  useEffect(() => {
    getMinimizeToTray().then((enabled) => {
      emit('minimize-to-tray-changed', { enabled }).catch(() => {});
    });
  }, []);

  const handleLogout = async () => {
    setLoggingOut(true);
    await logout();
    navigate('/login', { replace: true });
  };

  const handleReset = async () => {
    setResetting(true);
    await resetAuth();
    navigate('/setup', { replace: true });
  };

  const handleMinimizeToTrayChange = useCallback((checked: boolean) => {
    setMinimizeToTrayState(checked);
    setMinimizeToTray(checked);
    emit('minimize-to-tray-changed', { enabled: checked }).catch(() => {});
  }, []);

  const handleDenoiseToggle = useCallback(async (checked: boolean) => {
    setDenoiseEnabledState(checked);
    await setDenoiseEnabled(checked);
    await invoke('media_set_denoise_enabled', { enabled: checked });
    await setMediaDenoiseEnabled(checked);
  }, []);

  // Hotkey recording: capture keydown events when in recording mode
  useEffect(() => {
    if (!recordingHotkey) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();

      // Escape cancels recording
      if (e.key === 'Escape') {
        setRecordingHotkey(false);
        setRecordedModifiers([]);
        setRecordedKey(null);
        setHotkeyError(null);
        return;
      }

      // Collect modifiers
      const mods: string[] = [];
      if (e.ctrlKey) mods.push('Ctrl');
      if (e.shiftKey) mods.push('Shift');
      if (e.altKey) mods.push('Alt');
      if (e.metaKey) mods.push('Meta');

      // Ignore bare modifier presses
      if (['Control', 'Shift', 'Alt', 'Meta'].includes(e.key)) {
        setRecordedModifiers(mods);
        return;
      }

      // We have a main key — finalize the combo
      const mainKey = e.key.length === 1 ? e.key.toUpperCase() : e.key;
      setRecordedModifiers(mods);
      setRecordedKey(mainKey);

      const combo = formatHotkeyCombination(mods, mainKey);
      setRecordingHotkey(false);
      setRecordedModifiers([]);
      setRecordedKey(null);
      setHotkeyError(null);

      // Persist and register
      const oldHotkey = muteHotkey;
      setMuteHotkeyState(combo);
      setMuteHotkey(combo);

      // Try to re-register if currently registered (active session)
      isHotkeyRegistered(oldHotkey).then(async (wasRegistered) => {
        if (wasRegistered) {
          try {
            await unregisterMuteHotkey(oldHotkey);
            // Re-registration will happen via voice-room's hotkey change detection
          } catch {
            // best effort
          }
        }
      }).catch(() => {});
    };

    document.addEventListener('keydown', handleKeyDown, true);
    return () => document.removeEventListener('keydown', handleKeyDown, true);
  }, [recordingHotkey, muteHotkey]);

  // Watch All hotkey recording: capture keydown events when in recording mode
  useEffect(() => {
    if (!recordingWatchAllHotkey) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();

      if (e.key === 'Escape') {
        setRecordingWatchAllHotkey(false);
        setRecordedWatchAllModifiers([]);
        setRecordedWatchAllKey(null);
        setWatchAllHotkeyError(null);
        return;
      }

      const mods: string[] = [];
      if (e.ctrlKey) mods.push('Ctrl');
      if (e.shiftKey) mods.push('Shift');
      if (e.altKey) mods.push('Alt');
      if (e.metaKey) mods.push('Meta');

      if (['Control', 'Shift', 'Alt', 'Meta'].includes(e.key)) {
        setRecordedWatchAllModifiers(mods);
        return;
      }

      const mainKey = e.key.length === 1 ? e.key.toUpperCase() : e.key;
      setRecordedWatchAllModifiers(mods);
      setRecordedWatchAllKey(mainKey);

      const combo = formatHotkeyCombination(mods, mainKey);
      setRecordingWatchAllHotkey(false);
      setRecordedWatchAllModifiers([]);
      setRecordedWatchAllKey(null);
      setWatchAllHotkeyError(null);

      const oldHotkey = watchAllHotkey;
      setWatchAllHotkeyState(combo);
      setWatchAllHotkey(combo);

      isHotkeyRegistered(oldHotkey).then(async (wasRegistered) => {
        if (wasRegistered) {
          try {
            await unregisterWatchAllHotkey(oldHotkey);
          } catch {
            // best effort
          }
        }
      }).catch(() => {});
    };

    document.addEventListener('keydown', handleKeyDown, true);
    return () => document.removeEventListener('keydown', handleKeyDown, true);
  }, [recordingWatchAllHotkey, watchAllHotkey]);

  return (
    <div className="h-full flex flex-col min-w-0 bg-wavis-bg font-mono text-wavis-text">
      <div className="flex-shrink-0 px-3 sm:px-6 py-2 border-b border-wavis-text-secondary/30 bg-wavis-bg">
        {onClose ? (
          <button onClick={onClose} className="text-xs text-wavis-text-secondary border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast">
            ✕ /close settings
          </button>
        ) : (
          <button onClick={() => navigate('/')} className="text-xs text-wavis-text-secondary border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast">
            ← /channels
          </button>
        )}
      </div>
      {channelId && (
        <div className="flex shrink-0 border-b border-wavis-text-secondary font-mono text-xs">
          <button
            onClick={() => setActiveTab('general')}
            className={`px-4 py-1.5 transition-colors ${activeTab === 'general' ? 'text-wavis-accent border-b border-wavis-accent' : 'text-wavis-text-secondary hover:text-wavis-text'}`}
          >
            general
          </button>
          <button
            onClick={() => setActiveTab('channel')}
            className={`px-4 py-1.5 transition-colors ${activeTab === 'channel' ? 'text-wavis-accent border-b border-wavis-accent' : 'text-wavis-text-secondary hover:text-wavis-text'}`}
          >
            channel
          </button>
        </div>
      )}
      {channelId && activeTab === 'channel' ? (
        <div className="flex-1 min-h-0">
          <Suspense fallback={<div className="p-4 text-wavis-text-secondary">loading...</div>}>
            <ChannelDetail channelIdProp={channelId} hideJoinVoice={true} hideBackButton={true} />
          </Suspense>
        </div>
      ) : (
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-3 sm:px-6 py-6">
          <h2>settings</h2>
          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Device info */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">DEVICE</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-1 text-sm">
              <div>
                <span className="text-wavis-text-secondary">name: </span>
                <span>{displayName ?? '—'}</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">server: </span>
                <span>{serverUrl ?? '—'}</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">device id: </span>
                <span className="text-xs">{deviceId ? redactToken(deviceId, showSecrets) : '—'}</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">access token: </span>
                <span className="text-xs break-all">{accessToken ? redactToken(accessToken, showSecrets) : '—'}</span>
              </div>
            </div>
          </div>

          {/* Only show account management on standalone settings page */}
          {!onClose && (
            <>
              <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

              {/* Account management */}
              <div className="mb-6">
                <p className="text-sm text-wavis-text-secondary mb-2">ACCOUNT</p>
                <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-2">
                  <button
                    onClick={() => handleNavigateAway('/devices')}
                    className="block text-sm text-wavis-text hover:text-wavis-accent transition-colors"
                  >
                    /devices — manage devices
                  </button>
                  <button
                    onClick={() => handleNavigateAway('/pair')}
                    className="block text-sm text-wavis-text hover:text-wavis-accent transition-colors"
                  >
                    /pair-device — add a new device
                  </button>
                  <button
                    onClick={() => handleNavigateAway('/phrase')}
                    className="block text-sm text-wavis-text hover:text-wavis-accent transition-colors"
                  >
                    /change-password — change password
                  </button>
                  <div className="border-t border-wavis-text-secondary/30 pt-2 mt-2">
                    <button
                      onClick={handleLogout}
                      disabled={loggingOut}
                      className="block text-sm text-wavis-warn hover:text-wavis-danger transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                      {loggingOut ? 'logging out...' : '/logout — sign out of this device'}
                    </button>
                  </div>
                </div>
              </div>
            </>
          )}

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Profile color picker */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">PROFILE</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary">
              <p className="text-sm text-wavis-text-secondary mb-2">Color</p>
              <div className="flex flex-wrap gap-2">
                {PROFILE_COLORS.map((color) => (
                  <button
                    key={color}
                    className={`w-8 h-8 rounded${selectedColor === color ? ' ring-2 ring-wavis-accent' : ''}`}
                    style={{ backgroundColor: color }}
                    onClick={() => {
                      setSelectedColor(color);
                      setProfileColor(color);
                      updateSessionProfileColor(color);
                    }}
                    aria-label={`Select color ${color}`}
                  />
                ))}
              </div>
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Server config */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">SERVER</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
              <div>
                <span className="text-wavis-text-secondary">url: </span>
                <span>{serverUrl ?? '—'}</span>
              </div>
              <p className="text-xs text-wavis-text-secondary">Reset device to change server URL</p>
              {INSECURE_TLS_ALLOWED && (
              <>
              <div className="flex items-center justify-between">
                <span className="text-wavis-text-secondary">TLS verification</span>
                <Switch
                  checked={tlsEnabled}
                  onCheckedChange={(checked: boolean) => {
                    setTlsEnabled(checked);
                    setStoreValue(STORE_KEYS.tlsEnabled, checked);
                  }}
                  aria-label="Toggle TLS verification"
                />
              </div>
              {!tlsEnabled && (
                <p className="text-wavis-danger text-xs">
                  Insecure mode — connections are not verified
                </p>
              )}
              </>
              )}
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Audio devices & volume */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">AUDIO</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
              {audioError ? (
                <p className="text-wavis-danger text-xs">{audioError}</p>
              ) : (
                <>
                  <div>
                    <label htmlFor="audio-input" className="text-wavis-text-secondary block mb-1">Input device</label>
                    <select
                      id="audio-input"
                      value={selectedInputDevice}
                      onChange={(e) => {
                        const deviceId = e.target.value;
                        setSelectedInputDevice(deviceId);
                        setStoreValue(STORE_KEYS.audioInputDevice, deviceId);
                        setAudioDevice(deviceId, 'input').catch(() => {});
                      }}
                      className="w-full bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                    >
                      <option value="">System default</option>
                      {audioDevices
                        .filter((d) => d.kind === 'input')
                        .map((d) => (
                          <option key={d.id} value={d.id}>
                            {formatDeviceName(d)}
                          </option>
                        ))}
                    </select>
                  </div>
                  <div>
                    <label className="text-wavis-text-secondary block mb-1">Input volume</label>
                    <div className="flex items-center gap-3">
                      <div className="flex-1">
                        <VolumeSlider
                          value={inputVolume}
                          onChange={(v) => {
                            setInputVolumeState(v);
                            setInputVolume(v);
                            setAudioInputVolume(v).catch(() => {});
                          }}
                        />
                      </div>
                      <span className="text-wavis-text-secondary w-8 text-right tabular-nums">{inputVolume}</span>
                    </div>
                  </div>
                  <div>
                    <label htmlFor="audio-output" className="text-wavis-text-secondary block mb-1">Output device</label>
                    <select
                      id="audio-output"
                      value={selectedOutputDevice}
                      onChange={(e) => {
                        const deviceId = e.target.value;
                        setSelectedOutputDevice(deviceId);
                        setStoreValue(STORE_KEYS.audioOutputDevice, deviceId);
                        setAudioDevice(deviceId, 'output').catch(() => {});
                      }}
                      className="w-full bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                    >
                      <option value="">System default</option>
                      {audioDevices
                        .filter((d) => d.kind === 'output')
                        .map((d) => (
                          <option key={d.id} value={d.id}>
                            {formatDeviceName(d)}
                          </option>
                        ))}
                    </select>
                  </div>
                </>
              )}
              <div>
                <label className="text-wavis-text-secondary block mb-1">Master volume (down for maintenance)</label>
                <div className="flex items-center gap-3">
                  <div className="flex-1">
                    <VolumeSlider
                      value={volume}
                      onChange={() => {}}
                      disabled={true}
                      color={volume > 80 ? 'var(--wavis-danger)' : volume > 50 ? 'var(--wavis-warn)' : 'var(--wavis-accent)'}
                    />
                  </div>
                  <span className="text-wavis-text-secondary w-8 text-right tabular-nums">{volume}</span>
                </div>
              </div>
              <div>
                <label className="text-wavis-text-secondary block mb-1">Notification sounds volume</label>
                <div className="flex items-center gap-3">
                  <div className="flex-1">
                    <VolumeSlider
                      value={notificationVolume}
                      onChange={(v) => {
                        setNotificationVolumeState(v);
                        setNotificationVolume(v);
                        updateCachedNotificationVolume(v);
                      }}
                    />
                  </div>
                  <span className="text-wavis-text-secondary w-8 text-right tabular-nums">{notificationVolume}</span>
                </div>
              </div>
              <div>
                <button
                  onClick={() => setShowSoundVolumes(!showSoundVolumes)}
                  className="text-wavis-text-secondary text-sm flex items-center gap-1 hover:text-wavis-text"
                >
                  Per-sound volumes
                  <span className="text-xs">{showSoundVolumes ? '▲' : '▼'}</span>
                </button>
                {showSoundVolumes && (
                  <div className="mt-2 ml-3 space-y-2">
                    {SOUND_LABELS.map(({ key, label }) => {
                      const v = soundVolumes[key] ?? 100;
                      return (
                        <div key={key}>
                          <label className="text-wavis-text-secondary block mb-0.5 text-xs">{label}</label>
                          <div className="flex items-center gap-3">
                            <div className="flex-1">
                              <VolumeSlider
                                value={v}
                                onChange={(next_v) => {
                                  const next = { ...soundVolumes, [key]: next_v };
                                  setSoundVolumesState(next);
                                  setSoundVolumes(next);
                                  updateCachedSoundVolumes(next);
                                }}
                              />
                            </div>
                            <span className="text-wavis-text-secondary w-8 text-right tabular-nums text-xs">{v}</span>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
              <div className="flex items-center justify-between">
                <span className="text-wavis-text-secondary">Noise Suppression (RNNoise)</span>
                <Switch
                  checked={denoiseEnabled}
                  onCheckedChange={handleDenoiseToggle}
                  aria-label="Toggle noise suppression"
                />
              </div>
              <p className={`text-xs ${
                denoiseStatus.tone === 'active'
                  ? 'text-wavis-accent'
                  : denoiseStatus.tone === 'disabled'
                    ? 'text-wavis-text-secondary'
                    : 'text-wavis-warn'
              }`}>
                {denoiseStatus.message}
              </p>
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Hotkeys */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">HOTKEYS</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
              <div>
                <label className="text-wavis-text-secondary block mb-1">Mute toggle hotkey</label>
                <button
                  onClick={() => {
                    setRecordingHotkey(true);
                    setRecordedModifiers([]);
                    setRecordedKey(null);
                    setHotkeyError(null);
                  }}
                  className="w-full text-left bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                  aria-label="Record mute hotkey"
                >
                  {recordingHotkey
                    ? (recordedModifiers.length > 0
                        ? `${recordedModifiers.join('+')}+...`
                        : 'Press keys...')
                    : muteHotkey}
                </button>
              </div>
              {recordingHotkey && (
                <p className="text-xs text-wavis-text-secondary">Press Escape to cancel</p>
              )}
              {hotkeyError && (
                <p className="text-wavis-danger text-xs">{hotkeyError}</p>
              )}
              <div>
                <label className="text-wavis-text-secondary block mb-1">Watch All toggle hotkey</label>
                <button
                  onClick={() => {
                    setRecordingWatchAllHotkey(true);
                    setRecordedWatchAllModifiers([]);
                    setRecordedWatchAllKey(null);
                    setWatchAllHotkeyError(null);
                  }}
                  className="w-full text-left bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                  aria-label="Record watch all hotkey"
                >
                  {recordingWatchAllHotkey
                    ? (recordedWatchAllModifiers.length > 0
                        ? `${recordedWatchAllModifiers.join('+')}+...`
                        : 'Press keys...')
                    : watchAllHotkey}
                </button>
              </div>
              {recordingWatchAllHotkey && (
                <p className="text-xs text-wavis-text-secondary">Press Escape to cancel</p>
              )}
              {watchAllHotkeyError && (
                <p className="text-wavis-danger text-xs">{watchAllHotkeyError}</p>
              )}
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Notifications */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">NOTIFICATIONS</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
              {([
                ['participantJoined', 'Participant joined'],
                ['participantLeft', 'Participant left'],
                ['participantKicked', 'Participant kicked'],
                ['participantMutedByHost', 'Muted by host'],
                ['inviteReceived', 'Invite received'],
              ] as const).map(([key, label]) => (
                <div key={key} className="flex items-center justify-between">
                  <span className="text-wavis-text-secondary">{label}</span>
                  <Switch
                    checked={notifyToggles[key]}
                    onCheckedChange={(checked: boolean) => {
                      setNotifyToggles((prev) => ({ ...prev, [key]: checked }));
                      setNotificationToggle(key, checked);
                    }}
                  />
                </div>
              ))}
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* System tray */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">SYSTEM TRAY</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
              <div className="flex items-center justify-between">
                <span className="text-wavis-text-secondary">Minimize to tray on close</span>
                <Switch
                  checked={minimizeToTray}
                  onCheckedChange={handleMinimizeToTrayChange}
                  aria-label="Toggle minimize to tray"
                />
              </div>
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* About */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">ABOUT</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-1 text-sm">
              <div>
                <span className="text-wavis-text-secondary">version: </span>
                <span>{appVersion}</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">build: </span>
                <span>{import.meta.env.VITE_GIT_HASH || 'dev'}</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">platform: </span>
                <span>{osPlatform}</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">webview: </span>
                <span>{webviewVersion}</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">tauri: </span>
                <span>{tauriVersion}</span>
              </div>
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Credits */}
          <div className="mb-6">
            <p className="text-sm text-wavis-text-secondary mb-2">CREDITS</p>
            <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-1 text-sm">
              <div>
                <span className="text-wavis-text-secondary">sounds: </span>
                <span>Universfield, floraphonic, humordome, pixabay</span>
              </div>
              <div>
                <span className="text-wavis-text-secondary">source: </span>
                <button
                  onClick={() => open('https://pixabay.com')}
                  className="hover:text-wavis-accent hover:underline"
                >
                  pixabay.com
                </button>
              </div>
            </div>
          </div>

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Danger zone */}
          <div>
            <p className="text-sm text-wavis-danger mb-2">DANGER ZONE</p>
            <div className="p-3 bg-wavis-panel border border-wavis-danger space-y-3">
              <div>
                <p className="text-sm">Reset device registration</p>
                <p className="text-xs text-wavis-text-secondary mt-1">
                  Clears all tokens, device ID, and server URL. You will need to register again.
                  All channel memberships tied to this device will be lost.
                </p>
              </div>

              {!showConfirm ? (
                <button
                  onClick={() => setShowConfirm(true)}
                  className="border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors px-4 py-1 text-sm"
                >
                  /reset-device
                </button>
              ) : (
                <ConfirmTextGate
                  requiredText="RESET"
                  busy={resetting}
                  busyLabel="resetting..."
                  onConfirm={handleReset}
                  onCancel={() => setShowConfirm(false)}
                />
              )}
            </div>
          </div>
        </div>
      </div>
      )}
    </div>
  );
}
