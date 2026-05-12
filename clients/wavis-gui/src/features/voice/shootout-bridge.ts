/**
 * W4 codec-policy shootout bridge.
 *
 * Reads WAVIS_SHOOTOUT_* env vars from the Tauri backend, exposes the codec
 * override to livekit-media.ts, and handles periodic sample emission via
 * invoke('write_shootout_sample').
 */

import { invoke } from '@tauri-apps/api/core';
import type { ScreenShareCodecOverride } from '@features/settings/settings-store';

const DEBUG = import.meta.env.VITE_DEBUG_SHOOTOUT === 'true';
const LOG = '[wavis:shootout]';
const SAMPLE_INTERVAL_MS = 5_000;

export interface ShootoutEnvConfig {
  runId: string;
  cellId: string;
  codecPrimary: string;
  codecBackup: string;
  simulcast: boolean;
  profile: string;
  contentSource: string;
  participantCount: number;
  artifactDir: string;
}

// Module-level cache: undefined = not yet loaded, null = not a shootout run.
let _env: ShootoutEnvConfig | null | undefined = undefined;
let _samplingStop: (() => void) | null = null;

export async function loadShootoutEnv(): Promise<ShootoutEnvConfig | null> {
  if (_env !== undefined) return _env;
  try {
    _env = await invoke<ShootoutEnvConfig | null>('get_shootout_env');
  } catch {
    _env = null;
  }
  if (_env && DEBUG) console.info(LOG, 'shootout mode active', _env);
  return _env;
}

export function getShootoutEnv(): ShootoutEnvConfig | null {
  return _env ?? null;
}

export function isShootoutRun(): boolean {
  return _env != null;
}

/** Returns the codec override for this cell, or null if not a shootout run. */
export function getShootoutCodecOverride(): ScreenShareCodecOverride | null {
  if (!_env) return null;
  const p = _env.codecPrimary;
  if (p === 'vp9' || p === 'vp8' || p === 'av1') return p;
  return null;
}

/**
 * Starts emitting periodic resource samples to the per-cell JSONL artifact.
 * Call once the screen share is active. Returns a stop function.
 */
export function startShootoutSampling(env: ShootoutEnvConfig): () => void {
  if (_samplingStop) _samplingStop();

  const writeOnce = async (kind: string, extra?: Record<string, unknown>) => {
    try {
      await invoke('write_shootout_sample', {
        runId: env.runId,
        cellId: env.cellId,
        codecPrimary: env.codecPrimary,
        profile: env.profile,
        participantCount: env.participantCount,
        contentSource: env.contentSource,
        artifactDir: env.artifactDir,
        sampleKind: kind,
        extra: extra ? JSON.stringify(extra) : null,
      });
    } catch (err) {
      if (DEBUG) console.warn(LOG, 'write_shootout_sample failed', err);
    }
  };

  void writeOnce('share_started');

  const id = setInterval(() => void writeOnce('resource'), SAMPLE_INTERVAL_MS);

  const stop = () => {
    clearInterval(id);
    void writeOnce('share_stopped');
    _samplingStop = null;
  };

  _samplingStop = stop;
  if (DEBUG) console.info(LOG, `sampling started for cell ${env.cellId}`);
  return stop;
}

export function stopShootoutSampling(): void {
  if (_samplingStop) {
    _samplingStop();
    _samplingStop = null;
  }
}

/** Emit a single named event sample (downgrade, floor_hit, etc.). */
export function emitShootoutEvent(kind: string, extra?: Record<string, unknown>): void {
  const env = _env;
  if (!env) return;
  invoke('write_shootout_sample', {
    runId: env.runId,
    cellId: env.cellId,
    codecPrimary: env.codecPrimary,
    profile: env.profile,
    participantCount: env.participantCount,
    contentSource: env.contentSource,
    artifactDir: env.artifactDir,
    sampleKind: kind,
    extra: extra ? JSON.stringify(extra) : null,
  }).catch(() => {});
}
