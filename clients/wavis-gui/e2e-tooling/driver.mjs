// Dev-only launch/drive harness for the Wavis Tauri app. Not shipped — see
// README.md.
//
// Drives the real app via WebdriverIO's @wdio/tauri-service in standalone
// mode (startWdioSession/cleanupWdioSession) — genuine click/type/screenshot/
// DOM-read against the real app, on Windows, Linux, AND macOS. Windows uses
// the `embedded` driver provider (tauri-plugin-wdio-webdriver's in-process
// WebDriver server, gated behind the app's `e2e-webdriver` Cargo feature).
// macOS also uses the `embedded` provider — WKWebView has no standalone
// external WebDriver binary, so `tauri-plugin-wdio-webdriver` is the only
// supported path there too. Linux uses the `external` provider (tauri-driver +
// WebKitWebDriver, see README.md's Linux setup section) per this ticket's
// resolved decision to use the more standard/documented Linux path.
//
// Each Tauri window shows up as a distinct WebDriver window handle
// (browser.getWindowHandles()/switchToWindow()) — the multi-window
// equivalent of Playwright's old context.pages(). This is the ONLY
// multi-window mechanism used here: browser.tauri.switchWindow()/
// listWindows() require the separate tauri-plugin-wdio (execute/mock
// bridge), which is out of scope for this ticket — basic element
// interactions and window handles work without it.
import { existsSync } from 'node:fs';
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
 * Each launch gets a fresh, unique WEBVIEW2_USER_DATA_FOLDER-equivalent
 * profile via WAVIS_AUTH_STORE_NAME/WAVIS_KEYRING_SERVICE isolation (same
 * scheme as before) so it never conflicts with a normally-running Wavis
 * instance's profile, and multiple harness launches can coexist — including
 * TWO REAL launches of the same debug exe at once (e.g. a two-instance
 * live-backend spec): give each its own `port` and, since they'd otherwise
 * share one Tauri store file and one OS keychain service, its own
 * `authStoreName` / `keyringService` too. See README.md's "Two simultaneous
 * GUI instances" section.
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

  const capabilities = createTauriCapabilities(applicationPath, {
    driverProvider,
    embeddedPort,
    tauriDriverPort,
    autoInstallTauriDriver: driverProvider === 'external',
    // The embedded WebDriver server takes longer to come up on Windows CI-
    // like conditions; give it real headroom rather than racing the default.
    startTimeout: 60_000,
    env: {
      // WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS is a Windows/WebView2-only env
      // var. WKWebView (macOS) and WebKitGTK (Linux) ignore it entirely.
      //
      // --disable-features=WebRtcHideLocalIpsWithMdns: disables Chromium's
      // default mDNS obfuscation of local IPs in WebRTC ICE candidates, so
      // dockerized LiveKit can see real local IPs and connect. WebKit (both
      // macOS WKWebView and Linux WebKitGTK) does not implement mDNS
      // obfuscation at all — real IPs appear in ICE candidates by default on
      // both platforms, so no equivalent flag is needed there.
      //
      // --use-fake-ui-for-media-stream: auto-accepts getUserMedia permission
      // requests in WebView2 without a visible dialog. macOS uses TCC instead:
      // `npm run build:app` re-signs the binary with the com.wavis.desktop
      // bundle ID so TCC reuses the app's existing camera/mic grants; see
      // README.md's "macOS setup" section. No equivalent exists for Linux
      // WebKitGTK in the external-driver path (camera/mic specs are known-
      // failing on Linux for separate reasons — see README.md).
      ...(process.platform === 'win32' && {
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:
          '--disable-features=WebRtcHideLocalIpsWithMdns --use-fake-ui-for-media-stream',
      }),
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
    },
  });

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

  const getPageByPath = async (pathSubstring) => {
    const all = await pages();
    for (const p of all) {
      const url = await p.url();
      if (url.includes(pathSubstring)) return p;
    }
    const urls = await Promise.all(all.map((p) => p.url()));
    throw new Error(
      `No open window with URL containing "${pathSubstring}". Open: ${urls.join(', ') || '(none)'}`,
    );
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
