import { check, type DownloadEvent, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

export type UpdateCheckResult =
  | { kind: 'none' }
  | { kind: 'available'; update: Update }
  | { kind: 'error'; message: string };

export type UpdateProgress = {
  downloadedBytes: number;
  totalBytes: number | null;
};

export async function checkForUpdate(): Promise<UpdateCheckResult> {
  try {
    const update = await check({ timeout: 15_000 });
    return update ? { kind: 'available', update } : { kind: 'none' };
  } catch (err) {
    return {
      kind: 'error',
      message: err instanceof Error ? err.message : String(err),
    };
  }
}

export async function installUpdateAndRelaunch(
  update: Update,
  onProgress: (progress: UpdateProgress) => void,
): Promise<void> {
  let downloadedBytes = 0;
  let totalBytes: number | null = null;

  await update.downloadAndInstall((event: DownloadEvent) => {
    switch (event.event) {
      case 'Started':
        totalBytes = event.data.contentLength ?? null;
        downloadedBytes = 0;
        onProgress({ downloadedBytes, totalBytes });
        break;
      case 'Progress':
        downloadedBytes += event.data.chunkLength;
        onProgress({ downloadedBytes, totalBytes });
        break;
      case 'Finished':
        onProgress({ downloadedBytes: totalBytes ?? downloadedBytes, totalBytes });
        break;
    }
  });

  await relaunch();
}
