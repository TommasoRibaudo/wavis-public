// Dev-only launch/drive harness for the Wavis Tauri app. Not shipped — see
// README.md.
//
// Drives the real app via WebdriverIO's @wdio/tauri-service in standalone
// mode (startWdioSession/cleanupWdioSession) — genuine click/type/screenshot/
// DOM-read against the real app, on Windows AND Linux. Windows uses the
// `embedded` driver provider (tauri-plugin-wdio-webdriver's in-process
// WebDriver server, gated behind the app's `e2e-webdriver` Cargo feature).
// Linux uses the `external` provider (tauri-driver + WebKitWebDriver, see
// README.md's Linux setup section) per this ticket's resolved decision to
// use the more standard/documented Linux path.
//
// Each Tauri window shows up as a distinct WebDriver window handle
// (browser.getWindowHandles()/switchToWindow()) — the multi-window
// equivalent of Playwright's old context.pages(). This is the ONLY
// multi-window mechanism used here: browser.tauri.switchWindow()/
// listWindows() require the separate tauri-plugin-wdio (execute/mock
// bridge), which is out of scope for this ticket — basic element
// interactions and window handles work without it.
import { existsSync, mkdirSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startWdioSession, cleanupWdioSession, createTauriCapabilities } from '@wdio/tauri-service';
import { Page } from './page-adapter.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const GUI_ROOT = path.resolve(__dirname, '..');
// This crate is a member of the repo's Cargo workspace, so `cargo`/`tauri
// build` place output at the workspace root's target/ dir, not under
// src-tauri/target/ (that would only be true for a standalone crate).
const WORKSPACE_ROOT = path.resolve(GUI_ROOT, '..', '..');

export function debugBinaryPath() {
  const exe =
    process.platform === 'win32'
      ? path.join(WORKSPACE_ROOT, 'target', 'debug', 'wavis-gui.exe')
      : path.join(WORKSPACE_ROOT, 'target', 'debug', 'wavis-gui');
  if (!existsSync(exe)) {
    throw new Error(
      `Debug binary not found at ${exe}. Build it first: cd clients/wavis-gui/e2e-tooling && npm run build:app`,
    );
  }
  return exe;
}

const DEFAULT_EMBEDDED_PORT = 4445;
const DEFAULT_TAURI_DRIVER_PORT = 4444;

/**
 * Launch the Wavis app under WebdriverIO and return { browser, pages,
 * getPageByPath, page, close }. Always call close(), or the app process and
 * WebDriver session linger in the background.
 *
 * Each launch gets its own WEBVIEW2_USER_DATA_FOLDER (keyed by `port`) so it
 * never conflicts with a normally-running Wavis instance's profile — WebView2
 * refuses to open a second environment against a folder another process
 * already has open — and, since they'd otherwise share one Tauri store file
 * and one OS keychain service, its own `authStoreName` / `keyringService`
 * too. Together this is what lets multiple harness launches coexist,
 * including TWO REAL launches of the same debug exe at once (e.g. a
 * two-instance live-backend spec): give each its own `port`. See README.md's
 * "Two simultaneous GUI instances" section.
 *
 * `settleMs` (default 4000) is how long to wait after connecting before
 * windows/content are expected to be meaningfully rendered — the app talks
 * to a real backend and its UI reflects real connection state (e.g.
 * "connecting" -> "offline"/"joined"), so this isn't just a paint delay.
 */
export async function launchApp({
  applicationPath = debugBinaryPath(),
  port,
  settleMs = 4000,
  keyringService = 'com.wavis.gui.e2e',
  authStoreName,
} = {}) {
  const driverProvider = process.platform === 'linux' ? 'external' : 'embedded';
  const embeddedPort = port ?? DEFAULT_EMBEDDED_PORT;
  const tauriDriverPort = port ?? DEFAULT_TAURI_DRIVER_PORT;
  const linuxDataDir = path.join(os.tmpdir(), 'wavis-e2e-xdg', `port-${tauriDriverPort}`);
  if (process.platform === 'linux') mkdirSync(linuxDataDir, { recursive: true });

  // Everything below the `env` key on this options object — including `env`
  // itself and `embeddedPort` above it — is silently dropped by
  // @wdio/tauri-service@1.2.0's createTauriCapabilities: its implementation
  // only copies appArgs/tauriDriverPort/logLevel/commandTimeout/startTimeout/
  // driverProvider/autoInstallTauriDriver into the `wdio:tauriServiceOptions`
  // object it returns (verified directly against
  // node_modules/@wdio/tauri-service/dist/esm/index.js's createTauriCapabilities
  // and the getEmbeddedPort/startEmbeddedDriver functions that later read
  // `options.embeddedPort`/`options.env` from exactly that object — there is
  // no other path either field could reach the spawned process through).
  // Concretely this meant every launch fell back to the same default
  // embedded-WebDriver port regardless of the `port` argument (fatal for two
  // simultaneous instances — see two-instances.spec.mjs) AND every custom env
  // var below (auth store name, keyring service, WebRTC test flags,
  // diagnostics auto-open) was never actually applied, so every launch used
  // the build's single default auth store — the real cause of stale
  // wavis-auth-e2e.json cross-contaminating unrelated spec runs. Patched back
  // in below until upstream forwards these itself.
  const capabilities = createTauriCapabilities(applicationPath, {
    driverProvider,
    embeddedPort,
    tauriDriverPort,
    autoInstallTauriDriver: driverProvider === 'external',
    // The embedded WebDriver server takes longer to come up on Windows CI-
    // like conditions; give it real headroom rather than racing the default.
    startTimeout: 60_000,
  });
  capabilities['wdio:tauriServiceOptions'].embeddedPort = embeddedPort;
  if (process.env.E2E_CAPTURE_BACKEND_LOGS === '1') {
    capabilities['wdio:tauriServiceOptions'].captureBackendLogs = true;
    capabilities['wdio:tauriServiceOptions'].backendLogLevel = 'debug';
  }
  capabilities['wdio:tauriServiceOptions'].env = {
    // The embedded service overwrites this to "true" when it spawns the app.
    // Keep external-provider launches from accidentally enabling the plugin
    // through an inherited shell variable and colliding with WebKitWebDriver.
    WDIO_EMBEDDED_SERVER: 'false',
    // @wdio/tauri-service's standalone path otherwise assigns every Linux
    // launch the same /tmp/tauri-worker-standalone XDG profile. Separate
    // startWdioSession() calls (especially app + appB) then contend for the
    // same WebKit storage and Tauri app-data files. Key the profile by the
    // caller-selected driver port, just like the WebView2 profile below.
    ...(process.platform === 'linux'
      ? {
          XDG_DATA_HOME: linuxDataDir,
        }
      : {}),
    // --disable-features=WebRtcHideLocalIpsWithMdns: test-launches only —
    // exposes real local IPs in ICE host candidates instead of Chromium's
    // default randomly-generated ".local" mDNS obfuscation. Needed because
    // the dockerized LiveKit used by live-backend media specs can't resolve
    // those mDNS names (Docker's bridge network doesn't forward the UDP
    // multicast mDNS needs), which otherwise breaks all real WebRTC media
    // for the browser client. Must never move into tauri.conf.json —
    // production/normal dev launches keep the default privacy behavior.
    //
    // --use-fake-ui-for-media-stream: test-launches only — auto-accepts
    // getUserMedia permission requests, suppressing the WebView's own
    // mic/camera permission bar. Without this, every live-backend media
    // spec blocks on a manual click. "Fake" refers only to the permission
    // UI — capture still uses real devices.
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:
      '--disable-features=WebRtcHideLocalIpsWithMdns --use-fake-ui-for-media-stream',
    // WebView2's user-data folder otherwise defaults to a location keyed
    // purely off the Tauri app identifier (com.wavis.desktop) — shared by
    // EVERY build of this app on the machine, including a developer's own
    // long-running normal launch. A second WebView2 environment can't open
    // against a folder another process already has open with different
    // settings: confirmed via captureBackendLogs against a real run, the
    // failure is WebView2 HRESULT 0x8007139F ("group or resource is not in
    // the correct state") — not a crash in this app. Officially-supported
    // WebView2 Loader env var (not Tauri-specific), so it's the actual fix
    // rather than a workaround. Keyed by port so the two simultaneous
    // instances a two-instance spec launches don't collide with each other
    // either.
    WEBVIEW2_USER_DATA_FOLDER: path.join(os.tmpdir(), 'wavis-e2e-webview2', `port-${embeddedPort}`),
    // Keeps the OS keychain refresh-token entry (src-tauri/src/main.rs's
    // keyring_service()) separate from a developer's real persisted login
    // on this machine. Defaults to one shared e2e service; pass a distinct
    // `keyringService` per launch when running two real instances at once
    // so neither's token rotation can clobber the other's keychain entry.
    WAVIS_KEYRING_SERVICE: keyringService,
    // Runtime override for the Tauri store filename (auth.ts's
    // resolveStoreName, read via src-tauri/src/main.rs's
    // get_auth_store_name). Undefined `authStoreName` means "use the
    // build-time VITE_AUTH_STORE_NAME baked into the exe" — explicit ''
    // (not simply omitting the key) so a value inherited from the
    // caller's own shell environment can't leak through.
    WAVIS_AUTH_STORE_NAME: authStoreName ?? '',
    // Runtime auto-open for the diagnostics window — separate from the
    // build-time VITE_DIAGNOSTICS flag (build-app.mjs) which only
    // controls whether it's bundled at all. multi-window.spec.mjs relies
    // on it being open with no prior click needed.
    WAVIS_DIAGNOSTICS_WINDOW: '1',
  };

  const browser = await startWdioSession(capabilities);

  await new Promise((resolve) => setTimeout(resolve, settleMs));

  let activeHandle = null;
  const focus = async (handle) => {
    if (activeHandle === handle) return;
    await browser.switchToWindow(handle);
    activeHandle = handle;
  };

  const pages = async () => {
    const handles = await browser.getWindowHandles();
    return handles.map((handle) => new Page(browser, handle, focus));
  };

  const getPageByPath = async (pathSubstring, { timeout = 10_000 } = {}) => {
    const deadline = Date.now() + timeout;
    let urls = [];
    for (;;) {
      const all = await pages();
      urls = [];
      for (const p of all) {
        const url = await p.url();
        urls.push(url);
        if (url.includes(pathSubstring)) return p;
      }
      if (Date.now() > deadline) {
        throw new Error(
          `No open window with URL containing "${pathSubstring}" after ${timeout}ms. Open: ${urls.join(', ') || '(none)'}`,
        );
      }
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
  };

  const page = async () => {
    const all = await pages();
    for (const p of all) {
      const url = await p.url();
      if (url.includes('/room')) return p;
    }
    // Window handles aren't reliably semantic across providers: the
    // Windows embedded provider (tauri-plugin-wdio-webdriver) happens to
    // use the Tauri window label ('main', 'diagnostics') as the WebDriver
    // handle directly, but Linux's external provider (tauri-driver +
    // WebKitWebDriver) hands out opaque `page-<UUID>` handles that never
    // equal 'main' — confirmed directly: every live-backend spec's
    // joinChannelViaUi() timed out waiting for a "channels" heading
    // because page() silently fell back to positional all[0] and picked
    // the diagnostics window instead. Excluding the one window we *know*
    // isn't main (its URL contains '/diagnostics') works identically on
    // both providers and doesn't depend on handle semantics at all.
    for (const p of all) {
      const url = await p.url();
      if (!url.includes('/diagnostics')) return p;
    }
    return all.find((p) => p.handle === 'main') ?? all[0];
  };

  const close = async () => {
    try {
      await cleanupWdioSession(browser);
    } catch {
      // session may already be gone
    }
  };

  return { browser, pages, getPageByPath, page, close };
}
