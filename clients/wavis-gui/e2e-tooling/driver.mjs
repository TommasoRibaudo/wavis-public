// Dev-only launch/drive harness for the Wavis Tauri app. Not shipped — see
// README.md.
//
// Primary path: launch the exe with WebView2's remote-debugging port enabled
// and connect real Playwright to it over CDP (chromium.connectOverCDP). This
// gives genuine click/type/screenshot/DOM-read on the real app, including
// real multi-window support (each Tauri window shows up as its own
// Playwright `Page` in `context.pages()`). Confirmed working — see the CDP
// spike results referenced in README.md. This is NOT WebDriver: it bypasses
// the WebDriver-protocol translation layer entirely, which is where an
// earlier `tauri-driver` attempt hit a dead end (see
// webdriver-spike-DEAD-END.mjs).
//
// Fallback path: launchAppWin32() + listWindows()/screenshotWindow() drive
// the app at the OS/Win32 level (enumerate real windows, screenshot via
// PrintWindow) with no DOM interaction. Kept for cases CDP can't reach.

import { spawn, execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const GUI_ROOT = path.resolve(__dirname, '..');
// This crate is a member of the repo's Cargo workspace, so `cargo`/`tauri
// build` place output at the workspace root's target/ dir, not under
// src-tauri/target/ (that would only be true for a standalone crate).
const WORKSPACE_ROOT = path.resolve(GUI_ROOT, '..', '..');

export function debugBinaryPath() {
  const exe = path.join(WORKSPACE_ROOT, 'target', 'debug', 'wavis-gui.exe');
  if (!existsSync(exe)) {
    throw new Error(
      `Debug binary not found at ${exe}. Build it first: cd clients/wavis-gui && npx tauri build --debug`,
    );
  }
  return exe;
}

/* ─── CDP + Playwright (primary path — real click/type/screenshot) ────── */

async function connectWithRetry(port, maxAttempts, delayMs) {
  for (let i = 0; i < maxAttempts; i++) {
    try {
      return await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    } catch (err) {
      if (i === maxAttempts - 1) throw err;
      await new Promise((resolve) => setTimeout(resolve, delayMs));
    }
  }
  throw new Error('unreachable');
}

/**
 * Launch the Wavis app with WebView2's CDP debugging port enabled and
 * connect real Playwright to it. Returns { browser, context, pages, page,
 * getPageByPath, close }. Always call close(), or the app process and
 * Playwright connection linger in the background.
 *
 * Each launch gets a fresh, unique WEBVIEW2_USER_DATA_FOLDER (temp dir) so
 * it never conflicts with a normally-running Wavis instance's profile, and
 * multiple harness launches can coexist.
 *
 * `settleMs` (default 4000) is how long to wait after connecting before
 * windows/content are expected to be meaningfully rendered — the app talks
 * to a real backend and its UI reflects real connection state (e.g.
 * "connecting" -> "offline"/"joined"), so this isn't just a paint delay.
 */
export async function launchApp({
  applicationPath = debugBinaryPath(),
  port = 9222,
  settleMs = 4000,
} = {}) {
  const userDataDir = mkdtempSync(path.join(tmpdir(), 'wavis-cdp-'));
  const child = spawn(applicationPath, [], {
    env: {
      ...process.env,
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
      // getUserMedia permission requests, suppressing WebView2's own
      // mic/camera permission bar (Windows-level privacy settings are
      // already "Allow"; the prompts the suite hits are Chromium's, and
      // nothing in src-tauri handles them). Without this, every live-backend
      // media spec blocks on a manual click, and granting mid-tone was
      // observed to perturb the GUI's audio pipeline mid-assertion. "Fake"
      // refers only to the permission UI — capture still uses real devices.
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --disable-features=WebRtcHideLocalIpsWithMdns --use-fake-ui-for-media-stream`,
      WEBVIEW2_USER_DATA_FOLDER: userDataDir,
      // Keeps the OS keychain refresh-token entry (src-tauri/src/main.rs's
      // keyring_service()) separate from a developer's real persisted login
      // on this machine. The Tauri store JSON file itself (wavis-auth.json)
      // is isolated separately via the VITE_AUTH_STORE_NAME build-time env
      // var baked into the live-backend debug exe — see e2e-tooling/README.md.
      WAVIS_KEYRING_SERVICE: 'com.wavis.gui.e2e',
    },
    stdio: 'ignore',
  });
  const pid = child.pid;

  let browser;
  try {
    browser = await connectWithRetry(port, 20, 500);
  } catch (err) {
    try {
      process.kill(pid, 'SIGTERM');
    } catch {
      // already exited
    }
    throw err;
  }

  await new Promise((resolve) => setTimeout(resolve, settleMs));

  const context = browser.contexts()[0];

  const close = async () => {
    try {
      await browser.close();
    } catch {
      // connection may already be gone
    }
    try {
      process.kill(pid, 'SIGTERM');
    } catch {
      // already exited
    }
  };

  return {
    pid,
    browser,
    context,
    /** All open Playwright pages (one per Tauri window) right now. */
    pages: () => context.pages(),
    /** First page whose URL contains pathSubstring, e.g. "/watch-all", "/diagnostics", "/room". */
    getPageByPath: (pathSubstring) => {
      const found = context.pages().find((p) => p.url().includes(pathSubstring));
      if (!found) {
        throw new Error(
          `No open window with URL containing "${pathSubstring}". Open: ${
            context
              .pages()
              .map((p) => p.url())
              .join(', ') || '(none)'
          }`,
        );
      }
      return found;
    },
    /** The main window's page, if open. */
    page: () => context.pages().find((p) => p.url().includes('/room')) ?? context.pages()[0],
    close,
  };
}

/* ─── Win32 (fallback path — screenshot only, no DOM interaction) ──────── */

function runPs(scriptName, args) {
  const scriptPath = path.join(__dirname, scriptName);
  return execFileSync(
    'powershell',
    ['-NoProfile', '-NonInteractive', '-File', scriptPath, ...args],
    { encoding: 'utf8' },
  );
}

/**
 * Launch the Wavis app with no CDP/Playwright involvement at all — a plain
 * child process. Use this only if the CDP path (launchApp) can't reach a
 * window for some reason; otherwise prefer launchApp for real interaction.
 */
export async function launchAppWin32({ applicationPath = debugBinaryPath(), waitMs = 6000 } = {}) {
  const child = spawn(applicationPath, [], { stdio: 'ignore', detached: false });
  const pid = child.pid;

  await new Promise((resolve) => setTimeout(resolve, waitMs));

  const close = () => {
    try {
      process.kill(pid, 'SIGTERM');
    } catch {
      // already exited
    }
  };

  return {
    pid,
    close,
    listWindows: () => listWindows(pid),
    screenshotWindow: (hwnd, outPath) => screenshotWindow(hwnd, outPath),
    screenshotWindowByTitle: (titleSubstring, outPath) =>
      screenshotWindowByTitle(pid, titleSubstring, outPath),
  };
}

/** List this process's visible top-level windows as [{ hwnd, title }]. */
export function listWindows(pid) {
  const out = runPs('list-windows.ps1', ['-ProcessId', String(pid)]);
  return out
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const [hwnd, ...titleParts] = line.split('|');
      return { hwnd: hwnd.trim(), title: titleParts.join('|').trim() };
    });
}

/** Screenshot a specific window by its exact HWND (from listWindows()). */
export function screenshotWindow(hwnd, outPath) {
  runPs('capture-window.ps1', ['-Hwnd', String(hwnd), '-OutPath', outPath]);
  return outPath;
}

/** Find a window by case-insensitive title substring (e.g. "Watch All", "Diagnostics") and screenshot it. */
export function screenshotWindowByTitle(pid, titleSubstring, outPath) {
  const windows = listWindows(pid);
  const match = windows.find((w) => w.title.toLowerCase().includes(titleSubstring.toLowerCase()));
  if (!match) {
    throw new Error(
      `No window with title containing "${titleSubstring}" found. Open windows: ${windows.map((w) => `"${w.title}"`).join(', ') || '(none)'}`,
    );
  }
  return screenshotWindow(match.hwnd, outPath);
}
