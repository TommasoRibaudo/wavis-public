import { useState, useEffect, useCallback } from 'react';

import { useNavigate } from 'react-router';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getVersion, getTauriVersion } from '@tauri-apps/api/app';
import type { Update } from '@tauri-apps/plugin-updater';
import {
  resetAuth,
  logout,
  getServerUrl,
  getDeviceId,
  getUsername,
  updateUsername,
  getAccessToken,
  INSECURE_TLS_ALLOWED,
} from '@features/auth/auth';
import { PROFILE_COLORS } from '@shared/colors';
import {
  getProfileColor,
  setProfileColor,
  getStoreValue,
  setStoreValue,
  STORE_KEYS,
  getDefaultVolume,
  DEFAULT_VOLUME,
  getMinimizeToTray,
  setMinimizeToTray,
  getNotificationToggles,
  setNotificationToggle,
  getMuteHotkey,
  setMuteHotkey,
  DEFAULT_MUTE_HOTKEY,
  getWatchAllHotkey,
  setWatchAllHotkey,
  DEFAULT_WATCH_ALL_HOTKEY,
  getFocusMainHotkey,
  setFocusMainHotkey,
  DEFAULT_FOCUS_MAIN_HOTKEY,
  getDenoiseEnabled,
  setDenoiseEnabled,
  getNotificationVolume,
  setNotificationVolume,
  getSoundVolumes,
  setSoundVolumes,
  getInputVolume,
  setInputVolume,
  getVideoInputDevice,
  setVideoInputDevice,
} from './settings-store';
import {
  updateCachedNotificationVolume,
  updateCachedSoundVolumes,
} from '@features/voice/notification-sounds';
import {
  updateSessionProfileColor,
  updateSessionUsername,
  getState as getVoiceRoomState,
  changeSelectedCamera,
  setPassthroughEnabled,
  setPassthroughVolume,
  setPassthroughFilter,
  setPassthrough,
  clearPassthrough,
  leaveRoom,
} from '@features/voice/voice-room';
import { VolumeSlider } from '@shared/VolumeSlider';
import {
  setAudioDevice,
  setAudioInputVolume,
  setMediaDenoiseEnabled,
} from '@features/voice/audio-devices';
import type { NotificationToggles } from './settings-store';
import {
  checkForUpdate,
  installUpdateAndRelaunch,
  leaveActiveVoiceRoomForUpdate,
  type UpdateProgress,
  updateProgressLabel,
  updateProgressPercent,
} from '@shared/update-service';
import { redactToken } from '@shared/helpers';
import { useDebug } from '@shared/debug-context';
import {
  unregisterMuteHotkey,
  unregisterWatchAllHotkey,
  unregisterFocusMainHotkey,
  registerFocusMainHotkey,
  isHotkeyRegistered,
} from '@shared/hotkey-bridge';
import { Switch } from '../../components/ui/switch';
import { open } from '@tauri-apps/plugin-shell';
import { ConfirmTextGate } from '@shared/ConfirmTextGate';
import ChannelDetail from '@features/channels/ChannelDetail';
import { useHotkeyRecorder } from './useHotkeyRecorder';

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

type SettingsUpdateState =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'none' }
  | { kind: 'available'; update: Update }
  | { kind: 'installing'; update: Update; progress: UpdateProgress }
  | { kind: 'error'; message: string };

export function describeDenoiseStatus(params: {
  denoiseEnabled: boolean;
  connectionMode: 'livekit' | 'native' | undefined;
  mediaState: 'disconnected' | 'connecting' | 'connected' | 'reconnecting' | 'failed';
  userAgent: string;
  noiseSuppressionActive?: boolean;
}): DenoiseStatus {
  const { denoiseEnabled, connectionMode, mediaState, userAgent, noiseSuppressionActive } = params;
  const isMacOrWindows = /Macintosh|Windows/i.test(userAgent);
  const sessionActive =
    mediaState === 'connected' || mediaState === 'connecting' || mediaState === 'reconnecting';

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
      message:
        'Active on this session. Your microphone is using the Wavis JS noise suppression processor.',
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
      message: 'On. Will be active when you join a session.',
    };
  }

  return {
    tone: 'saved',
    message: 'On. Applies on native Rust audio sessions; JS fallback sessions may not use it.',
  };
}

/* ─── Constants ─────────────────────────────────────────────────── */
const DIVIDER = '─'.repeat(48);

const SOUND_LABELS: { key: string; label: string }[] = [
  { key: 'join', label: 'Participant joined' },
  { key: 'leave', label: 'Participant left' },
  { key: 'chat', label: 'Chat message' },
  { key: 'share-start', label: 'Screen share started' },
  { key: 'share-stop', label: 'Screen share stopped' },
  { key: 'viewer-joined', label: 'Someone joined your stream' },
  { key: 'mute', label: 'You muted' },
  { key: 'unmute', label: 'You unmuted' },
  { key: 'deafen', label: 'You deafened' },
  { key: 'undeafen', label: 'You undeafened' },
];

/* ═══ Component ═════════════════════════════════════════════════════ */
interface SettingsProps {
  onClose?: () => void;
  onNavigateAway?: (path: string) => void;
  channelId?: string;
}
export default function Settings({ onClose, onNavigateAway, channelId }: SettingsProps = {}) {
  const navigate = useNavigate();
  const [activeTab, setActiveTab] = useState<'general' | 'channel'>('general');

  const handleNavigateAway = useCallback(
    (path: string) => {
      if (onNavigateAway) onNavigateAway(path);
      else void navigate(path);
    },
    [onNavigateAway, navigate],
  );

  const handleAuthNavigateAway = useCallback(
    (path: string) => {
      if (onNavigateAway) onNavigateAway(path);
      else void navigate(path, { replace: true });
    },
    [onNavigateAway, navigate],
  );
  const { showSecrets } = useDebug();

  const [showConfirm, setShowConfirm] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);
  const [serverUrl, setServerUrl] = useState<string | null>(null);
  const [deviceId, setDeviceId] = useState<string | null>(null);
  const [username, setUsernameState] = useState<string>('');
  const [usernameSaving, setUsernameSaving] = useState(false);
  const [usernameError, setUsernameError] = useState<string | null>(null);
  const [selectedColor, setSelectedColor] = useState<string>(PROFILE_COLORS[0]);
  const [tlsEnabled, setTlsEnabled] = useState(true);
  const [audioDevices, setAudioDevices] = useState<AudioDevice[]>([]);
  const [audioError, setAudioError] = useState<string | null>(null);
  const [selectedInputDevice, setSelectedInputDevice] = useState<string>('');
  const [videoInputDevices, setVideoInputDevices] = useState<MediaDeviceInfo[]>([]);
  const [selectedVideoDevice, setSelectedVideoDevice] = useState<string | null>(null);
  const supportedCapturePlatform = /Windows|Macintosh/.test(navigator.userAgent);
  const [selectedOutputDevice, setSelectedOutputDevice] = useState<string>('');
  const [volume, setVolume] = useState<number>(DEFAULT_VOLUME);
  const [accessToken, setAccessTokenVal] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState<string>('—');
  const [tauriVersion, setTauriVersion] = useState<string>('—');
  const [osPlatform, setOsPlatform] = useState<string>('—');
  const [webviewVersion, setWebviewVersion] = useState<string>('—');
  const [settingsUpdateState, setSettingsUpdateState] = useState<SettingsUpdateState>({
    kind: 'idle',
  });
  const [minimizeToTray, setMinimizeToTrayState] = useState(false);
  const [notifyToggles, setNotifyToggles] = useState<NotificationToggles>({
    participantJoined: true,
    participantLeft: true,
    participantKicked: true,
    participantMutedByHost: true,
    inviteReceived: true,
    passthroughChanged: true,
  });
  const [passthroughVolume, setPassthroughVolumeState] = useState<number>(
    getVoiceRoomState().passthroughVolume,
  );
  const [passthroughFiltersEnabled, setPassthroughFiltersEnabledState] = useState<boolean>(
    getVoiceRoomState().passthroughFiltersEnabled,
  );
  const [passthroughFilterStrength, setPassthroughFilterStrengthState] = useState<number>(
    getVoiceRoomState().passthroughFilterStrength,
  );
  const [muteHotkey, setMuteHotkeyState] = useState<string>(DEFAULT_MUTE_HOTKEY);
  const [watchAllHotkey, setWatchAllHotkeyState] = useState<string>(DEFAULT_WATCH_ALL_HOTKEY);
  const [denoiseEnabled, setDenoiseEnabledState] = useState(true);
  const [inputVolume, setInputVolumeState] = useState<number>(100);
  const [notificationVolume, setNotificationVolumeState] = useState<number>(50);
  const [soundVolumes, setSoundVolumesState] = useState<Record<string, number>>({});
  const [showSoundVolumes, setShowSoundVolumes] = useState(false);
  const [focusMainHotkey, setFocusMainHotkeyState] = useState<string>(DEFAULT_FOCUS_MAIN_HOTKEY);
  const denoiseStatus = describeDenoiseStatus({
    denoiseEnabled,
    connectionMode: getVoiceRoomState().connectionMode,
    mediaState: getVoiceRoomState().mediaState,
    userAgent: navigator.userAgent,
    noiseSuppressionActive: getVoiceRoomState().noiseSuppressionActive,
  });
  const availableSettingsUpdate =
    settingsUpdateState.kind === 'available' ? settingsUpdateState.update : null;
  const settingsUpdateProgress =
    settingsUpdateState.kind === 'installing' ? settingsUpdateState.progress : null;
  const settingsUpdatePercent = settingsUpdateProgress
    ? updateProgressPercent(settingsUpdateProgress)
    : null;

  useEffect(() => {
    void getServerUrl().then(setServerUrl);
    void getDeviceId().then(setDeviceId);
    void getUsername().then((name) => setUsernameState(name ?? ''));
    void getProfileColor().then(setSelectedColor);
    void getStoreValue(STORE_KEYS.tlsEnabled, true).then(setTlsEnabled);
    void getDefaultVolume().then(setVolume);
    void getStoreValue<string>(STORE_KEYS.audioInputDevice, '').then(setSelectedInputDevice);
    void getStoreValue<string>(STORE_KEYS.audioOutputDevice, '').then(setSelectedOutputDevice);
    void getAccessToken().then(setAccessTokenVal);
    void getMinimizeToTray().then(setMinimizeToTrayState);
    void getNotificationToggles().then(setNotifyToggles);
    setPassthroughVolumeState(getVoiceRoomState().passthroughVolume);
    setPassthroughFiltersEnabledState(getVoiceRoomState().passthroughFiltersEnabled);
    setPassthroughFilterStrengthState(getVoiceRoomState().passthroughFilterStrength);
    void getMuteHotkey().then(setMuteHotkeyState);
    void getWatchAllHotkey().then(setWatchAllHotkeyState);
    void getFocusMainHotkey().then(setFocusMainHotkeyState);
    void getDenoiseEnabled().then(setDenoiseEnabledState);
    void getInputVolume().then(setInputVolumeState);
    void getNotificationVolume().then(setNotificationVolumeState);
    void getSoundVolumes().then(setSoundVolumesState);
    getVersion()
      .then(setAppVersion)
      .catch(() => {});
    getTauriVersion()
      .then(setTauriVersion)
      .catch(() => {});
    setOsPlatform(navigator.platform || 'unknown');
    const ua = navigator.userAgent;
    const webkitMatch = ua.match(/AppleWebKit\/([\d.]+)/);
    const chromeMatch = ua.match(/Chrome\/([\d.]+)/);
    setWebviewVersion(chromeMatch?.[1] ?? webkitMatch?.[1] ?? 'unknown');
    invoke<AudioDevice[]>('list_audio_devices')
      .then(setAudioDevices)
      .catch(() => setAudioError('Failed to load audio devices'));
    void getVideoInputDevice().then(setSelectedVideoDevice);
    if (supportedCapturePlatform) {
      navigator.mediaDevices
        .enumerateDevices()
        .then((devs) => setVideoInputDevices(devs.filter((d) => d.kind === 'videoinput')))
        .catch(() => {});
    }
  }, [supportedCapturePlatform]);

  // Startup sync: emit minimize-to-tray-changed to Rust after store loads
  useEffect(() => {
    void getMinimizeToTray().then((enabled) => {
      emit('minimize-to-tray-changed', { enabled }).catch(() => {});
    });
  }, []);

  const handleLogout = async () => {
    setLoggingOut(true);
    leaveRoom();
    await logout();
    handleAuthNavigateAway('/login');
  };

  const handleReset = async () => {
    setResetting(true);
    leaveRoom();
    await resetAuth();
    handleAuthNavigateAway('/setup');
  };

  const handleMinimizeToTrayChange = useCallback((checked: boolean) => {
    setMinimizeToTrayState(checked);
    void setMinimizeToTray(checked);
    emit('minimize-to-tray-changed', { enabled: checked }).catch(() => {});
  }, []);

  const handleDenoiseToggle = useCallback(async (checked: boolean) => {
    setDenoiseEnabledState(checked);
    await setDenoiseEnabled(checked);
    await invoke('media_set_denoise_enabled', { enabled: checked });
    await setMediaDenoiseEnabled(checked);
  }, []);

  const handleCheckForUpdate = useCallback(() => {
    setSettingsUpdateState({ kind: 'checking' });
    void checkForUpdate().then((result) => {
      if (result.kind === 'available') {
        setSettingsUpdateState({ kind: 'available', update: result.update });
      } else if (result.kind === 'none') {
        setSettingsUpdateState({ kind: 'none' });
      } else {
        setSettingsUpdateState({ kind: 'error', message: result.message });
      }
    });
  }, []);

  const handleInstallSettingsUpdate = useCallback(() => {
    if (!availableSettingsUpdate) return;
    const update = availableSettingsUpdate;

    setSettingsUpdateState({
      kind: 'installing',
      update,
      progress: { downloadedBytes: 0, totalBytes: null },
    });

    void (async () => {
      await leaveActiveVoiceRoomForUpdate();
      await installUpdateAndRelaunch(update, (progress) => {
        setSettingsUpdateState({ kind: 'installing', update, progress });
      });
    })().catch((err) => {
      setSettingsUpdateState({
        kind: 'error',
        message: err instanceof Error ? err.message : String(err),
      });
    });
  }, [availableSettingsUpdate]);

  const handleUsernameSave = useCallback(async () => {
    const trimmed = username.trim();
    if (!trimmed) {
      setUsernameError('username is required');
      return;
    }
    if (trimmed.length > 64) {
      setUsernameError('username must be 64 characters or less');
      return;
    }
    setUsernameSaving(true);
    setUsernameError(null);
    try {
      await updateUsername(trimmed);
      setUsernameState(trimmed);
      updateSessionUsername(trimmed);
    } catch (err) {
      setUsernameError(err instanceof Error ? err.message : 'failed to save username');
    } finally {
      setUsernameSaving(false);
    }
  }, [username]);

  const muteRecorder = useHotkeyRecorder({
    hotkey: muteHotkey,
    setHotkeyState: setMuteHotkeyState,
    persist: setMuteHotkey,
    unregister: unregisterMuteHotkey,
    isRegistered: isHotkeyRegistered,
  });

  const watchAllRecorder = useHotkeyRecorder({
    hotkey: watchAllHotkey,
    setHotkeyState: setWatchAllHotkeyState,
    persist: setWatchAllHotkey,
    unregister: unregisterWatchAllHotkey,
    isRegistered: isHotkeyRegistered,
  });

  // Re-register new hotkey immediately so it works without reconnecting
  const handleFocusMainReRegistered = useCallback((combo: string) => {
    void registerFocusMainHotkey(combo, () => {
      void getCurrentWindow()
        .unminimize()
        .then(() => getCurrentWindow().setFocus());
    });
  }, []);

  const focusMainRecorder = useHotkeyRecorder({
    hotkey: focusMainHotkey,
    setHotkeyState: setFocusMainHotkeyState,
    persist: setFocusMainHotkey,
    unregister: unregisterFocusMainHotkey,
    isRegistered: isHotkeyRegistered,
    onReRegistered: handleFocusMainReRegistered,
  });

  return (
    <div className="h-full flex flex-col min-w-0 bg-wavis-bg font-mono text-wavis-text">
      {channelId && (
        <div className="flex shrink-0 h-[4.5rem] px-3 py-3 border-b border-wavis-text-secondary font-mono text-xs items-center justify-between">
          <div className="flex items-center h-full">
            <button
              onClick={() => setActiveTab('general')}
              className={`px-4 h-full flex items-center transition-colors ${activeTab === 'general' ? 'text-wavis-accent border-b border-wavis-accent' : 'text-wavis-text-secondary hover:text-wavis-text'}`}
            >
              general
            </button>
            <button
              onClick={() => setActiveTab('channel')}
              className={`px-4 h-full flex items-center transition-colors ${activeTab === 'channel' ? 'text-wavis-accent border-b border-wavis-accent' : 'text-wavis-text-secondary hover:text-wavis-text'}`}
            >
              channel
            </button>
          </div>
          {onClose ? (
            <button
              onClick={onClose}
              className="text-wavis-text-secondary hover:text-wavis-text transition-colors text-xs px-1"
              aria-label="Close settings"
            >
              [x]
            </button>
          ) : (
            <button
              onClick={() => {
                void navigate('/');
              }}
              className="text-xs text-wavis-text-secondary border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
            >
              ← /channels
            </button>
          )}
        </div>
      )}
      {channelId && activeTab === 'channel' ? (
        <div className="flex-1 overflow-y-auto">
          <div className="max-w-2xl mx-auto px-3 sm:px-6 py-4 space-y-4">
            <ChannelDetail
              channelIdProp={channelId}
              hideJoinVoice
              hideBackButton
              embedded
              onNavigateAway={handleNavigateAway}
              embeddedMiddle={(() => {
                const vs = getVoiceRoomState();
                if (!vs.selfIsHost || vs.machineState !== 'active') return null;
                const activePassthrough = vs.passthrough;
                const passthroughEnabled = vs.passthroughEnabled;
                const showPassthroughControls = passthroughEnabled || !!activePassthrough;
                const joinedSubRoomId = vs.joinedSubRoomId;
                const otherSubRooms = vs.subRooms.filter((r) => r.id !== joinedSubRoomId);
                return (
                  <div>
                    <p className="text-sm text-wavis-text-secondary mb-2">PASSTHROUGH</p>
                    <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
                      <div className="flex items-center justify-between">
                        <span className="text-wavis-text-secondary">Enable passthrough</span>
                        <Switch
                          checked={passthroughEnabled}
                          onCheckedChange={setPassthroughEnabled}
                          aria-label="Toggle passthrough"
                        />
                      </div>
                      <p className="text-xs text-wavis-text-secondary/70">
                        Link two rooms so their participants can hear each other
                      </p>
                      {showPassthroughControls && (
                        <>
                          <div>
                            <label className="text-wavis-text-secondary block mb-1">
                              Passthrough volume ({passthroughVolume}%)
                            </label>
                            <p className="text-xs text-wavis-text-secondary/70 mb-1">
                              Attenuates audio from the linked room — applies to all participants
                            </p>
                            <VolumeSlider
                              value={passthroughVolume}
                              onChange={(v) => {
                                setPassthroughVolumeState(v);
                                setPassthroughVolume(v);
                              }}
                            />
                          </div>
                          <div className="space-y-2">
                            <div className="flex items-center justify-between">
                              <span className="text-wavis-text-secondary">
                                Muffle passthrough voices
                              </span>
                              <Switch
                                checked={passthroughFiltersEnabled}
                                onCheckedChange={(checked: boolean) => {
                                  setPassthroughFiltersEnabledState(checked);
                                  setPassthroughFilter(checked, passthroughFilterStrength);
                                }}
                                aria-label="Toggle passthrough muffle"
                              />
                            </div>
                            <div>
                              <label className="text-wavis-text-secondary block mb-1">
                                Muffle strength ({passthroughFilterStrength}%)
                              </label>
                              <p className="text-xs text-wavis-text-secondary/70 mb-1">
                                {passthroughFilterStrength < 25
                                  ? 'Subtle'
                                  : passthroughFilterStrength < 75
                                    ? 'Balanced'
                                    : 'Strong'}
                              </p>
                              <VolumeSlider
                                value={passthroughFilterStrength}
                                onChange={(v) => {
                                  setPassthroughFilterStrengthState(v);
                                  setPassthroughFilter(passthroughFiltersEnabled, v);
                                }}
                              />
                            </div>
                          </div>
                          <div>
                            {activePassthrough ? (
                              <div className="flex items-center justify-between">
                                <div>
                                  <span className="text-wavis-text-secondary">Linked</span>
                                  <p className="text-xs text-wavis-text-secondary/70">
                                    {activePassthrough.label}
                                  </p>
                                </div>
                                <button
                                  type="button"
                                  onClick={() => {
                                    clearPassthrough();
                                  }}
                                  className="text-xs px-2 py-0.5 border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors"
                                >
                                  /unlink
                                </button>
                              </div>
                            ) : passthroughEnabled &&
                              joinedSubRoomId &&
                              otherSubRooms.length > 0 ? (
                              <div>
                                <span className="text-wavis-text-secondary block mb-1">
                                  Link to room
                                </span>
                                <div className="flex flex-wrap gap-1">
                                  {otherSubRooms.map((r) => (
                                    <button
                                      key={r.id}
                                      type="button"
                                      onClick={() => {
                                        setPassthrough(r.id);
                                      }}
                                      className="text-xs px-2 py-0.5 border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors"
                                    >
                                      Room {r.roomNumber}
                                    </button>
                                  ))}
                                </div>
                              </div>
                            ) : passthroughEnabled ? (
                              <p className="text-xs text-wavis-text-secondary/70">
                                Join a room to link rooms
                              </p>
                            ) : null}
                          </div>
                        </>
                      )}
                    </div>
                  </div>
                );
              })()}
            />
          </div>
        </div>
      ) : (
        <div className="flex-1 overflow-y-auto">
          <div className="max-w-2xl mx-auto px-3 sm:px-6 py-6">
            {!channelId && onClose ? (
              <button
                onClick={onClose}
                className="mb-4 text-wavis-text-secondary hover:text-wavis-text transition-colors text-xs px-1"
                aria-label="Close settings"
              >
                [x]
              </button>
            ) : !channelId ? (
              <button
                onClick={() => {
                  void navigate('/');
                }}
                className="mb-4 text-xs text-wavis-text-secondary border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
              >
                ← /channels
              </button>
            ) : null}
            <h2>settings</h2>
            <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

            {/* Account info */}
            <div className="mb-6">
              <p className="text-sm text-wavis-text-secondary mb-2">ACCOUNT</p>
              <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-1 text-sm">
                <div>
                  <span className="text-wavis-text-secondary">username: </span>
                  <span>{username || '—'}</span>
                </div>
                <div>
                  <span className="text-wavis-text-secondary">server: </span>
                  <span>{serverUrl ?? '—'}</span>
                </div>
                <div>
                  <span className="text-wavis-text-secondary">device id: </span>
                  <span className="text-xs">
                    {deviceId ? redactToken(deviceId, showSecrets) : '—'}
                  </span>
                </div>
                <div>
                  <span className="text-wavis-text-secondary">access token: </span>
                  <span className="text-xs break-all">
                    {accessToken ? redactToken(accessToken, showSecrets) : '—'}
                  </span>
                </div>
              </div>
            </div>

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
                    onClick={() => {
                      void handleLogout();
                    }}
                    disabled={loggingOut}
                    className="block text-sm text-wavis-warn hover:text-wavis-danger transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    {loggingOut ? 'logging out...' : '/logout — sign out of this device'}
                  </button>
                </div>
              </div>
            </div>

            <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

            {/* Profile color picker */}
            <div className="mb-6">
              <p className="text-sm text-wavis-text-secondary mb-2">PROFILE</p>
              <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3">
                <div>
                  <label
                    htmlFor="username"
                    className="text-sm text-wavis-text-secondary block mb-1"
                  >
                    Username
                  </label>
                  <div className="flex gap-2">
                    <input
                      id="username"
                      value={username}
                      maxLength={64}
                      onChange={(e) => {
                        setUsernameState(e.target.value);
                        setUsernameError(null);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') void handleUsernameSave();
                      }}
                      className="flex-1 min-w-0 bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                    />
                    <button
                      onClick={() => {
                        void handleUsernameSave();
                      }}
                      disabled={usernameSaving}
                      className="border border-wavis-accent text-wavis-accent hover:bg-wavis-accent hover:text-wavis-bg transition-colors px-3 py-1 text-sm disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                      {usernameSaving ? 'saving...' : '/save'}
                    </button>
                  </div>
                  {usernameError && (
                    <p className="text-xs text-wavis-danger mt-1">{usernameError}</p>
                  )}
                </div>
                <div>
                  <p className="text-sm text-wavis-text-secondary mb-2">Color</p>
                  <div className="flex flex-wrap gap-2">
                    {PROFILE_COLORS.map((color) => (
                      <button
                        key={color}
                        className={`w-8 h-8 rounded${selectedColor === color ? ' ring-2 ring-wavis-accent' : ''}`}
                        style={{ backgroundColor: color }}
                        onClick={() => {
                          setSelectedColor(color);
                          void setProfileColor(color);
                          updateSessionProfileColor(color);
                        }}
                        aria-label={`Select color ${color}`}
                      />
                    ))}
                  </div>
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
                <p className="text-xs text-wavis-text-secondary">
                  Reset device to change server URL
                </p>
                {INSECURE_TLS_ALLOWED && (
                  <>
                    <div className="flex items-center justify-between">
                      <span className="text-wavis-text-secondary">TLS verification</span>
                      <Switch
                        checked={tlsEnabled}
                        onCheckedChange={(checked: boolean) => {
                          setTlsEnabled(checked);
                          void setStoreValue(STORE_KEYS.tlsEnabled, checked);
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
                      <label htmlFor="audio-input" className="text-wavis-text-secondary block mb-1">
                        Input device
                      </label>
                      <select
                        id="audio-input"
                        value={selectedInputDevice}
                        onChange={(e) => {
                          const deviceId = e.target.value;
                          setSelectedInputDevice(deviceId);
                          void setStoreValue(STORE_KEYS.audioInputDevice, deviceId);
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
                              void setInputVolume(v);
                              setAudioInputVolume(v).catch(() => {});
                            }}
                          />
                        </div>
                        <span className="text-wavis-text-secondary w-8 text-right tabular-nums">
                          {inputVolume}
                        </span>
                      </div>
                    </div>
                    <div>
                      <label
                        htmlFor="audio-output"
                        className="text-wavis-text-secondary block mb-1"
                      >
                        Output device
                      </label>
                      <select
                        id="audio-output"
                        value={selectedOutputDevice}
                        onChange={(e) => {
                          const deviceId = e.target.value;
                          setSelectedOutputDevice(deviceId);
                          void setStoreValue(STORE_KEYS.audioOutputDevice, deviceId);
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
                  <label className="text-wavis-text-secondary block mb-1">
                    Master volume (down for maintenance)
                  </label>
                  <div className="flex items-center gap-3">
                    <div className="flex-1">
                      <VolumeSlider
                        value={volume}
                        onChange={() => {}}
                        disabled={true}
                        color={
                          volume > 80
                            ? 'var(--wavis-danger)'
                            : volume > 50
                              ? 'var(--wavis-warn)'
                              : 'var(--wavis-accent)'
                        }
                      />
                    </div>
                    <span className="text-wavis-text-secondary w-8 text-right tabular-nums">
                      {volume}
                    </span>
                  </div>
                </div>
                <div>
                  <label className="text-wavis-text-secondary block mb-1">
                    Notification sounds volume
                  </label>
                  <div className="flex items-center gap-3">
                    <div className="flex-1">
                      <VolumeSlider
                        value={notificationVolume}
                        onChange={(v) => {
                          setNotificationVolumeState(v);
                          void setNotificationVolume(v);
                          updateCachedNotificationVolume(v);
                        }}
                      />
                    </div>
                    <span className="text-wavis-text-secondary w-8 text-right tabular-nums">
                      {notificationVolume}
                    </span>
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
                            <label className="text-wavis-text-secondary block mb-0.5 text-xs">
                              {label}
                            </label>
                            <div className="flex items-center gap-3">
                              <div className="flex-1">
                                <VolumeSlider
                                  value={v}
                                  onChange={(next_v) => {
                                    const next = { ...soundVolumes, [key]: next_v };
                                    setSoundVolumesState(next);
                                    void setSoundVolumes(next);
                                    updateCachedSoundVolumes(next);
                                  }}
                                />
                              </div>
                              <span className="text-wavis-text-secondary w-8 text-right tabular-nums text-xs">
                                {v}
                              </span>
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
                    onCheckedChange={(checked) => {
                      void handleDenoiseToggle(checked);
                    }}
                    aria-label="Toggle noise suppression"
                  />
                </div>
                <p
                  className={`text-xs ${
                    denoiseStatus.tone === 'active'
                      ? 'text-wavis-accent'
                      : denoiseStatus.tone === 'disabled'
                        ? 'text-wavis-text-secondary'
                        : 'text-wavis-warn'
                  }`}
                >
                  {denoiseStatus.message}
                </p>
              </div>
            </div>

            <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

            {/* Video settings */}
            <div className="mb-6">
              <p className="text-sm text-wavis-text-secondary mb-2">VIDEO</p>
              <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
                {!supportedCapturePlatform ? (
                  <p className="text-wavis-text-secondary text-xs">
                    Camera capture is not supported on this platform.
                  </p>
                ) : videoInputDevices.length === 0 ? (
                  <>
                    <p className="text-wavis-text-secondary text-xs">No camera detected.</p>
                    <select
                      disabled
                      className="w-full bg-wavis-bg border border-wavis-text-secondary text-wavis-text-secondary font-mono text-sm px-2 py-1 outline-none opacity-50 cursor-not-allowed"
                    >
                      <option>No cameras available</option>
                    </select>
                  </>
                ) : videoInputDevices.length === 1 ? (
                  <div>
                    <label className="text-wavis-text-secondary block mb-1">Camera</label>
                    <div className="w-full bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1">
                      {videoInputDevices[0].label || 'Camera 1'}
                    </div>
                  </div>
                ) : (
                  <div>
                    <label htmlFor="video-input" className="text-wavis-text-secondary block mb-1">
                      Camera
                    </label>
                    <select
                      id="video-input"
                      value={selectedVideoDevice ?? ''}
                      onChange={(e) => {
                        const deviceId = e.target.value || null;
                        setSelectedVideoDevice(deviceId);
                        void setVideoInputDevice(deviceId);
                        const voiceState = getVoiceRoomState();
                        if (voiceState.cameraPublication === 'published') {
                          void changeSelectedCamera(deviceId);
                        }
                      }}
                      className="w-full bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                    >
                      <option value="">Default camera</option>
                      {videoInputDevices.map((d, i) => (
                        <option key={d.deviceId} value={d.deviceId}>
                          {d.label || `Camera ${i + 1}`}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
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
                    onClick={muteRecorder.startRecording}
                    className="w-full text-left bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                    aria-label="Record mute hotkey"
                  >
                    {muteRecorder.recording
                      ? muteRecorder.modifiers.length > 0
                        ? `${muteRecorder.modifiers.join('+')}+...`
                        : 'Press keys...'
                      : muteHotkey}
                  </button>
                </div>
                {muteRecorder.recording && (
                  <p className="text-xs text-wavis-text-secondary">Press Escape to cancel</p>
                )}
                {muteRecorder.error && (
                  <p className="text-wavis-danger text-xs">{muteRecorder.error}</p>
                )}
                <div>
                  <label className="text-wavis-text-secondary block mb-1">
                    Watch All toggle hotkey
                  </label>
                  <button
                    onClick={watchAllRecorder.startRecording}
                    className="w-full text-left bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                    aria-label="Record watch all hotkey"
                  >
                    {watchAllRecorder.recording
                      ? watchAllRecorder.modifiers.length > 0
                        ? `${watchAllRecorder.modifiers.join('+')}+...`
                        : 'Press keys...'
                      : watchAllHotkey}
                  </button>
                </div>
                {watchAllRecorder.recording && (
                  <p className="text-xs text-wavis-text-secondary">Press Escape to cancel</p>
                )}
                {watchAllRecorder.error && (
                  <p className="text-wavis-danger text-xs">{watchAllRecorder.error}</p>
                )}
                <div>
                  <label className="text-wavis-text-secondary block mb-1">
                    Focus main window hotkey
                  </label>
                  <button
                    onClick={focusMainRecorder.startRecording}
                    className="w-full text-left bg-wavis-bg border border-wavis-text-secondary text-wavis-text font-mono text-sm px-2 py-1 outline-none focus:border-wavis-accent"
                    aria-label="Record focus main hotkey"
                  >
                    {focusMainRecorder.recording
                      ? focusMainRecorder.modifiers.length > 0
                        ? `${focusMainRecorder.modifiers.join('+')}+...`
                        : 'Press keys...'
                      : focusMainHotkey}
                  </button>
                </div>
                {focusMainRecorder.recording && (
                  <p className="text-xs text-wavis-text-secondary">Press Escape to cancel</p>
                )}
                {focusMainRecorder.error && (
                  <p className="text-wavis-danger text-xs">{focusMainRecorder.error}</p>
                )}
              </div>
            </div>

            <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

            {/* Notifications */}
            <div className="mb-6">
              <p className="text-sm text-wavis-text-secondary mb-2">NOTIFICATIONS</p>
              <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-3 text-sm">
                {(
                  [
                    ['participantJoined', 'Participant joined'],
                    ['participantLeft', 'Participant left'],
                    ['participantKicked', 'Participant kicked'],
                    ['participantMutedByHost', 'Muted by host'],
                    ['inviteReceived', 'Invite received'],
                    ['passthroughChanged', 'Passthrough changed'],
                  ] as const
                ).map(([key, label]) => (
                  <div key={key} className="flex items-center justify-between">
                    <span className="text-wavis-text-secondary">{label}</span>
                    <Switch
                      checked={notifyToggles[key]}
                      onCheckedChange={(checked: boolean) => {
                        setNotifyToggles((prev) => ({ ...prev, [key]: checked }));
                        void setNotificationToggle(key, checked);
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
                <div className="pt-2 space-y-2">
                  <div className="flex flex-wrap items-center gap-2">
                    <button
                      type="button"
                      onClick={handleCheckForUpdate}
                      disabled={
                        settingsUpdateState.kind === 'checking' ||
                        settingsUpdateState.kind === 'installing'
                      }
                      className="border border-wavis-accent px-3 py-1 text-xs text-wavis-accent transition-colors hover:bg-wavis-accent hover:text-wavis-bg disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-wavis-accent"
                    >
                      {settingsUpdateState.kind === 'checking' ? 'checking...' : '/check-update'}
                    </button>
                    {availableSettingsUpdate && (
                      <button
                        type="button"
                        onClick={handleInstallSettingsUpdate}
                        className="border border-wavis-accent px-3 py-1 text-xs text-wavis-accent transition-colors hover:bg-wavis-accent hover:text-wavis-bg"
                      >
                        /install-update
                      </button>
                    )}
                  </div>
                  {settingsUpdateState.kind === 'none' && (
                    <p className="text-xs text-wavis-text-secondary">Wavis is up to date.</p>
                  )}
                  {settingsUpdateState.kind === 'available' && (
                    <p className="text-xs text-wavis-accent">
                      Wavis {settingsUpdateState.update.version} is available.
                    </p>
                  )}
                  {settingsUpdateState.kind === 'installing' && (
                    <div className="space-y-1">
                      <p className="text-xs text-wavis-text-secondary">
                        {updateProgressLabel(settingsUpdateState.progress)}
                      </p>
                      {settingsUpdatePercent !== null && (
                        <div className="h-1 overflow-hidden bg-wavis-text-secondary/20">
                          <div
                            className="h-full bg-wavis-accent transition-all duration-300"
                            style={{ width: `${settingsUpdatePercent}%` }}
                          />
                        </div>
                      )}
                    </div>
                  )}
                  {settingsUpdateState.kind === 'error' && (
                    <p className="text-xs text-wavis-danger break-all">
                      {settingsUpdateState.message}
                    </p>
                  )}
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
                  <span>Universfield, floraphonic, humordome, pixabay, SoundReality - </span>
                  <button
                    onClick={() => {
                      void open('https://pixabay.com');
                    }}
                    className="hover:text-wavis-accent hover:underline"
                  >
                    pixabay.com
                  </button>
                </div>
                <div>
                  <span className="text-wavis-text-secondary">icons: </span>
                  <span>video camera by Kiranshastry — </span>
                  <button
                    onClick={() => {
                      void open('https://www.flaticon.com/free-icons/video-camera');
                    }}
                    className="hover:text-wavis-accent hover:underline"
                  >
                    flaticon.com
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
                    onConfirm={() => {
                      void handleReset();
                    }}
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
