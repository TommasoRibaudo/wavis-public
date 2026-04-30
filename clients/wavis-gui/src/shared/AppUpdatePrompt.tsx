import { useEffect, useState } from 'react';
import type { Update } from '@tauri-apps/plugin-updater';
import { getState } from '@features/voice/voice-room';
import {
  checkForUpdate,
  installUpdateAndRelaunch,
  type UpdateProgress,
} from './update-service';

type PromptState =
  | { kind: 'idle' }
  | { kind: 'available'; update: Update }
  | { kind: 'installing'; update: Update; progress: UpdateProgress }
  | { kind: 'deferred'; update: Update }
  | { kind: 'error'; message: string };

function isVoiceRoomActive(): boolean {
  const state = getState();
  return (
    state.machineState === 'active' ||
    state.mediaState === 'connecting' ||
    state.mediaState === 'connected'
  );
}

function progressLabel(progress: UpdateProgress): string {
  if (!progress.totalBytes) return 'Downloading update...';

  const percent = Math.min(
    100,
    Math.round((progress.downloadedBytes / progress.totalBytes) * 100),
  );
  return `Downloading update... ${percent}%`;
}

export default function AppUpdatePrompt() {
  const [promptState, setPromptState] = useState<PromptState>({ kind: 'idle' });

  useEffect(() => {
    let cancelled = false;

    const timeoutId = window.setTimeout(() => {
      void checkForUpdate().then((result) => {
        if (cancelled) return;
        if (result.kind === 'available') {
          setPromptState({ kind: 'available', update: result.update });
        } else if (result.kind === 'error') {
          console.warn('[wavis:update] update check failed:', result.message);
        }
      });
    }, 5_000);

    return () => {
      cancelled = true;
      window.clearTimeout(timeoutId);
    };
  }, []);

  if (promptState.kind === 'idle') return null;

  const update = promptState.kind === 'error' ? null : promptState.update;
  const versionText = update ? `Wavis ${update.version} is available.` : 'Update failed.';

  const install = () => {
    if (!update) return;
    if (isVoiceRoomActive()) {
      setPromptState({ kind: 'deferred', update });
      return;
    }

    setPromptState({
      kind: 'installing',
      update,
      progress: { downloadedBytes: 0, totalBytes: null },
    });

    void installUpdateAndRelaunch(update, (progress) => {
      setPromptState({ kind: 'installing', update, progress });
    }).catch((err) => {
      setPromptState({
        kind: 'error',
        message: err instanceof Error ? err.message : String(err),
      });
    });
  };

  const body =
    promptState.kind === 'installing'
      ? progressLabel(promptState.progress)
      : promptState.kind === 'deferred'
        ? 'Leave the active room before installing the update.'
        : promptState.kind === 'error'
          ? promptState.message
          : 'Install when you are ready to restart.';

  const isError = promptState.kind === 'error';
  const isInstalling = promptState.kind === 'installing';
  const isDeferred = promptState.kind === 'deferred';

  return (
    <div className="fixed right-4 bottom-4 z-50 w-[min(360px,calc(100vw-2rem))] border border-wavis-text-secondary/30 bg-wavis-panel p-4 font-mono text-sm text-wavis-text shadow-2xl">
      <div className="flex items-center gap-2">
        <span className={isError ? 'text-wavis-danger' : 'text-wavis-accent'}>▸</span>
        <span className="font-semibold">{versionText}</span>
      </div>
      <div className="mt-1 pl-5 text-wavis-text-secondary">
        {body}
      </div>
      {isInstalling && promptState.progress.totalBytes != null && (
        <div className="mt-2 ml-5 h-1 overflow-hidden bg-wavis-text-secondary/20">
          <div
            className="h-full bg-wavis-accent transition-all duration-300"
            style={{
              width: `${Math.min(100, Math.round((promptState.progress.downloadedBytes / promptState.progress.totalBytes) * 100))}%`,
            }}
          />
        </div>
      )}
      <div className="mt-3 flex justify-end gap-2">
        {!isInstalling && (
          <button
            type="button"
            className="border border-wavis-text-secondary px-2 py-0.5 text-xs text-wavis-text transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
            onClick={() => setPromptState({ kind: 'idle' })}
          >
            /later
          </button>
        )}
        {(promptState.kind === 'available' || isDeferred) && (
          <button
            type="button"
            className="border border-wavis-accent px-2 py-0.5 text-xs text-wavis-accent transition-colors hover:bg-wavis-accent hover:text-wavis-bg"
            onClick={install}
          >
            /update
          </button>
        )}
      </div>
    </div>
  );
}
