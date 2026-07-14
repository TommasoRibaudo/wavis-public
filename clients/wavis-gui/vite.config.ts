import { defineConfig } from 'vite';
import { configDefaults } from 'vitest/config';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';
import { fileURLToPath } from 'node:url';

const host = process.env.TAURI_DEV_HOST;
const WINDOWS_SHARE_PATH_OVERRIDE_KEY = 'VITE_WAVIS_WINDOWS_SHARE_PATH';
const rootDir = fileURLToPath(new URL('.', import.meta.url));

export default defineConfig(({ command, mode }) => {
  if (
    command === 'build' &&
    mode === 'production' &&
    Object.prototype.hasOwnProperty.call(process.env, WINDOWS_SHARE_PATH_OVERRIDE_KEY)
  ) {
    throw new Error(`${WINDOWS_SHARE_PATH_OVERRIDE_KEY} must be unset for production builds`);
  }

  return {
    plugins: [react(), tailwindcss()],
    build: {
      target: ['safari15'],
      rollupOptions: {
        output: {
          manualChunks(id) {
            if (
              id.includes('/node_modules/livekit-client/') ||
              id.includes('\\node_modules\\livekit-client\\')
            ) {
              return 'livekit-client';
            }
          },
        },
      },
    },
    resolve: {
      alias: {
        '@shared': path.resolve(rootDir, './src/shared'),
        '@features': path.resolve(rootDir, './src/features'),
      },
    },
    clearScreen: false,
    optimizeDeps: {
      // Wavis patches livekit-client in postinstall. If Vite prebundles it into
      // node_modules/.vite, dev runs can keep serving a stale cached copy until
      // that cache is manually deleted. Exclude it so dev always reads the
      // patched ESM bundle directly from node_modules.
      exclude: ['livekit-client'],
    },
    server: {
      port: 1420,
      strictPort: true,
      host: host || false,
      hmr: host ? { protocol: 'ws', host, port: 1421 } : undefined,
      watch: { ignored: ['**/src-tauri/**'] },
    },
    test: {
      setupFiles: ['./vitest.setup.ts'],
      environment: 'node',
      env: {
        VITE_ALLOW_INSECURE_TLS: 'true',
      },
      // e2e-tooling has its own Playwright Test runner and *.spec.mjs files —
      // exclude it or vitest's default glob tries to collect them too.
      exclude: [...configDefaults.exclude, 'e2e-tooling/**'],
    },
  };
});
