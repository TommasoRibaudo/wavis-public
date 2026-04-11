import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';
import { fileURLToPath } from 'url';

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@shared': path.resolve(__dirname, './src/shared'),
      '@features': path.resolve(__dirname, './src/features'),
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
  build: {
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        compatProbe: fileURLToPath(new URL('./compat-probe.html', import.meta.url)),
      },
    },
  },
  test: {
    setupFiles: ['./vitest.setup.ts'],
    environment: 'node',
    env: {
      VITE_ALLOW_INSECURE_TLS: 'true',
    },
  },
});
