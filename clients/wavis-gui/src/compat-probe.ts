import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { load } from '@tauri-apps/plugin-store';

type CompatCapabilityStatus = {
  status: 'available_by_os' | 'skipped' | 'unknown' | 'not_applicable';
  detail: string;
};

type CompatCheckResult = {
  ipc_ok: boolean;
  audio_devices: Array<{
    id: string;
    name: string;
    kind: string;
    is_default: boolean;
  }>;
  store_ok: boolean;
  screen_capture_kit?: CompatCapabilityStatus;
  audio_process_tap?: CompatCapabilityStatus;
  notes: string[];
};

async function probeStore(): Promise<{ ok: boolean; note?: string }> {
  try {
    const store = await load('wavis-compat-probe.json', { defaults: {}, autoSave: false });
    const value = new Date().toISOString();
    await store.set('last_probe_at', value);
    const roundTrip = await store.get<string>('last_probe_at');
    await store.delete('last_probe_at');
    await store.save();
    return { ok: roundTrip === value };
  } catch (err) {
    return { ok: false, note: err instanceof Error ? err.message : String(err) };
  }
}

async function main(): Promise<void> {
  let result: CompatCheckResult;

  try {
    result = await invoke<CompatCheckResult>('__compat_check');
  } catch (err) {
    result = {
      ipc_ok: false,
      audio_devices: [],
      store_ok: false,
      notes: [`__compat_check failed: ${err instanceof Error ? err.message : String(err)}`],
    };
  }

  const store = await probeStore();
  result = {
    ...result,
    store_ok: store.ok,
    notes: store.note ? [...result.notes, `store probe failed: ${store.note}`] : result.notes,
  };

  await invoke('__compat_write_result', { result });
  await getCurrentWindow().close();
}

main().catch(async (err) => {
  const message = err instanceof Error ? err.message : String(err);
  try {
    await invoke('__compat_write_result', {
      result: {
        ipc_ok: false,
        audio_devices: [],
        store_ok: false,
        notes: [`compat probe failed: ${message}`],
      },
    });
  } finally {
    await getCurrentWindow().close();
  }
});
