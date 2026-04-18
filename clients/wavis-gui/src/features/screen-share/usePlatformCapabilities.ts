import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface PlatformCapabilities {
  hasScreenCaptureKit: boolean;
  hasProcessTap: boolean;
}

const FALLBACK: PlatformCapabilities = { hasScreenCaptureKit: false, hasProcessTap: false };

export function usePlatformCapabilities(): PlatformCapabilities {
  const [caps, setCaps] = useState<PlatformCapabilities>(FALLBACK);

  useEffect(() => {
    invoke<{ has_screen_capture_kit: boolean; has_process_tap: boolean }>(
      'get_platform_capabilities'
    )
      .then(({ has_screen_capture_kit, has_process_tap }) => {
        setCaps({ hasScreenCaptureKit: has_screen_capture_kit, hasProcessTap: has_process_tap });
      })
      .catch(() => {
        // On error keep the safe default (all false = features disabled).
      });
  }, []);

  return caps;
}
