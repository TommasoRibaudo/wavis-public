import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

export type AudioDriverState =
  | 'checking'
  | 'installed'
  | 'not_installed'
  | 'browser_opened'
  | 'install_failed';

export interface AudioDriverInstall {
  driverState: AudioDriverState;
  installError: string | null;
  triggerInstall: () => Promise<boolean>;
}

/**
 * Checks whether a supported virtual audio loopback device (BlackHole etc.)
 * is installed, and opens the BlackHole download page when requested.
 * Only active on macOS (controlled by the `enabled` flag).
 * On Windows/Linux the Rust stub returns true, so driverState is always
 * 'installed' there.
 */
export function useAudioDriverInstall(enabled: boolean): AudioDriverInstall {
  const [driverState, setDriverState] = useState<AudioDriverState>('checking');
  const [installError, setInstallError] = useState<string | null>(null);

  useEffect(() => {
    if (!enabled) {
      setDriverState('installed');
      return;
    }
    invoke<boolean>('check_audio_driver')
      .then((installed) => {
        setDriverState(installed ? 'installed' : 'not_installed');
      })
      .catch(() => {
        // Treat any IPC error as installed so the prompt never blocks unexpectedly.
        setDriverState('installed');
      });
  }, [enabled]);

  const triggerInstall = useCallback(async (): Promise<boolean> => {
    setInstallError(null);
    try {
      await invoke('install_audio_driver');
      setDriverState('installed');
      return true;
    } catch (err) {
      const errStr = err instanceof Error ? err.message : String(err);
      if (errStr === 'manual_install_required') {
        setDriverState('browser_opened');
      } else {
        setDriverState('install_failed');
        setInstallError(errStr);
      }
      return false;
    }
  }, []);

  return { driverState, installError, triggerInstall };
}
