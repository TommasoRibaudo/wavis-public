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

  return (
    <div className="fixed right-4 bottom-4 z-50 w-[min(360px,calc(100vw-2rem))] rounded-xl border border-white/10 bg-[#111814] p-4 text-sm text-white shadow-2xl">
      <div className="font-semibold">{versionText}</div>
      <div className="mt-1 text-white/70">{body}</div>
      <div className="mt-4 flex justify-end gap-2">
        {promptState.kind !== 'installing' && (
          <button
            type="button"
            className="rounded-md px-3 py-2 text-white/70 hover:bg-white/10 hover:text-white"
            onClick={() => setPromptState({ kind: 'idle' })}
          >
            Later
          </button>
        )}
        {(promptState.kind === 'available' || promptState.kind === 'deferred') && (
          <button
            type="button"
            className="rounded-md bg-[#d7ff73] px-3 py-2 font-semibold text-[#132016] hover:bg-[#c7ee64]"
            onClick={install}
          >
            Restart to update
          </button>
        )}
      </div>
    </div>
  );
}
