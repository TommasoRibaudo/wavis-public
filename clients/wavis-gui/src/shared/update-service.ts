import { check, type DownloadEvent, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { invoke } from '@tauri-apps/api/core';

export type UpdateCheckResult =
  | { kind: 'none' }
  | { kind: 'available'; update: Update }
  | { kind: 'error'; message: string };

export type UpdateProgress = {
  downloadedBytes: number;
  totalBytes: number | null;
};

type UpdateBinaryInfo = {
  path: string;
  size: number | null;
  modifiedMs: number | null;
};

type UpdateInstallContext = {
  version: string;
  currentBinary: UpdateBinaryInfo | null;
};

function isAppImage(binary: UpdateBinaryInfo | null): boolean {
  return binary?.path.endsWith('.AppImage') ?? false;
}

function sameBinaryFile(before: UpdateBinaryInfo | null, after: UpdateBinaryInfo | null): boolean {
  return (
    before !== null &&
    after !== null &&
    before.path === after.path &&
    before.size === after.size &&
    before.modifiedMs === after.modifiedMs
  );
}

async function getUpdateInstallContext(): Promise<UpdateInstallContext | null> {
  try {
    return await invoke<UpdateInstallContext>('get_update_install_context');
  } catch (err) {
    console.warn('[wavis:update] install context unavailable:', err);
    return null;
  }
}

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
  const before = await getUpdateInstallContext();

  console.info('[wavis:update] installing update', {
    currentVersion: before?.version ?? null,
    updateVersion: update.version,
    currentBinary: before?.currentBinary ?? null,
  });

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

  const after = await getUpdateInstallContext();
  console.info('[wavis:update] install command completed', {
    currentVersion: after?.version ?? null,
    updateVersion: update.version,
    currentBinaryBefore: before?.currentBinary ?? null,
    currentBinaryAfter: after?.currentBinary ?? null,
  });

  if (
    isAppImage(before?.currentBinary ?? null) &&
    sameBinaryFile(before?.currentBinary ?? null, after?.currentBinary ?? null)
  ) {
    throw new Error(
      `Update install finished but did not modify ${before?.currentBinary?.path}. ` +
        'Move the AppImage to a writable local folder and try again.',
    );
  }

  await relaunch();
}
