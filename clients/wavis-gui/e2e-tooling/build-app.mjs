// Builds the debug app binary with the e2e-webdriver Cargo feature enabled,
// and with the build-time env vars the spec suite needs — historically a
// developer's own ambient `clients/wavis-gui/.env` supplied these (that file
// is gitignored and never present in a fresh worktree, which is exactly the
// footgun this wrapper exists to remove):
// - VITE_ALLOW_INSECURE_TLS / VITE_AUTH_STORE_NAME / VITE_ALLOW_SERVER_OVERRIDE
//   — see README.md's "Live-backend specs" section. Without these, the
//   binary silently authenticates against the real default backend using a
//   real persisted session instead of the local docker-compose stack.
// - VITE_DIAGNOSTICS=true — bundles the diagnostics window at all;
//   multi-window.spec.mjs uses it as its always-open second window. The
//   runtime auto-open flag (WAVIS_DIAGNOSTICS_WINDOW) is separate and set
//   per-launch by driver.mjs's launchApp(), not baked in here.
//
// Tauri's build script validates every `.json` file under `capabilities/`
// against the plugin ACL compiled into the binary, regardless of whether
// that capability is in the active `app.security.capabilities` allowlist.
// Since `wdio-webdriver:default` only exists when the `e2e-webdriver`
// feature is on, the capability file can never sit in `capabilities/`
// during a plain `cargo check`/`cargo build` (no dev, no CI, no Stop hook).
// So it's copied in from a template just for this build, then removed
// again immediately after — success or failure — to keep that window shut.
import { copyFileSync, existsSync, rmSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const GUI_ROOT = path.resolve(__dirname, '..');
// src-tauri is a workspace member, so cargo places output at the workspace
// root's target/, not under src-tauri/target/.
const WORKSPACE_ROOT = path.resolve(GUI_ROOT, '..', '..');
const TEMPLATE = path.join(GUI_ROOT, 'src-tauri', 'e2e-webdriver-capability.template.json');
const CAPABILITY_FILE = path.join(GUI_ROOT, 'src-tauri', 'capabilities', 'e2e-webdriver.json');

copyFileSync(TEMPLATE, CAPABILITY_FILE);

let exitCode = 1;
try {
  const result = spawnSync(
    'npx',
    [
      'tauri',
      'build',
      '--debug',
      '--features',
      'e2e-webdriver',
      '--config',
      'src-tauri/tauri.e2e.conf.json',
      '--no-bundle',
    ],
    {
      cwd: GUI_ROOT,
      stdio: 'inherit',
      shell: process.platform === 'win32',
      env: {
        ...process.env,
        VITE_ALLOW_INSECURE_TLS: 'true',
        VITE_AUTH_STORE_NAME: 'wavis-auth-e2e.json',
        VITE_ALLOW_SERVER_OVERRIDE: 'true',
        VITE_DIAGNOSTICS: 'true',
      },
    },
  );
  exitCode = result.status ?? 1;
} finally {
  if (existsSync(CAPABILITY_FILE)) rmSync(CAPABILITY_FILE);
}

if (exitCode === 0 && process.platform === 'darwin') {
  // tauri build --debug --no-bundle on macOS produces an ad-hoc linker-signed
  // flat binary whose TCC identity is an auto-generated hash ("wavis_gui-<hex>"),
  // not the configured bundle identifier ("com.wavis.desktop"). macOS TCC tracks
  // camera/mic/etc. grants by bundle identifier, so every getUserMedia call from
  // the unidentified binary would trigger a new TCC dialog — blocking automated
  // test runs. Re-signing with the real identifier makes TCC reuse whatever
  // camera/mic grants are already stored for com.wavis.desktop (granted by the
  // production app or by the first-run dialog — see README.md's macOS setup).
  // Ad-hoc signing (--sign -) is sufficient for local development; no Developer
  // ID certificate is required.
  const binary = path.join(WORKSPACE_ROOT, 'target', 'debug', 'wavis-gui');
  const entitlements = path.join(GUI_ROOT, 'src-tauri', 'Entitlements.plist');
  const resign = spawnSync(
    'codesign',
    ['--force', '--sign', '-', '--identifier', 'com.wavis.desktop', '--entitlements', entitlements, binary],
    { stdio: 'inherit' },
  );
  exitCode = resign.status ?? 1;
}

process.exit(exitCode);
