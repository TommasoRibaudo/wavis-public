// Diagnostic script kept from the Phase 0 WebDriver feasibility spike.
//
// Result: tauri-driver v2.0.6 + msedgedriver 150.0.4078.65 connects
// successfully (one window handle, session stays alive), but the browsing
// context it attaches to never leaves `about:blank` — readyState is
// "complete" and document.documentElement.outerHTML is a genuinely empty
// <html><head></head><body></body></html>, even after 15+ seconds of
// polling. The real app window (confirmed via direct launch + Win32
// PrintWindow capture, see driver.mjs) renders correctly and immediately —
// so this is a tauri-driver/WebView2 attachment issue, not an app bug.
// Only one window handle was ever reported despite the app opening two
// real windows (main + diagnostics, VITE_DIAGNOSTICS=true in this debug
// build) — so multi-window control was never even reachable.
//
// Conclusion: DOM-level WebDriver interaction is not usable against this
// app/toolchain combination today. driver.mjs uses the Win32
// launch+enumerate+PrintWindow path instead (screenshots only, no
// scripted click/type) — see e2e-tooling/README.md "Known limitation".
//
// Kept for reference / re-running if a future tauri-driver or Tauri
// release fixes this — not part of the working harness. Requires
// `tauri-driver --port 4444` running separately before you run this.

import { remote } from 'webdriverio';

const browser = await remote({
  hostname: '127.0.0.1',
  port: 4444,
  path: '/',
  logLevel: 'warn',
  capabilities: {
    browserName: 'wry',
    'tauri:options': {
      application: 'C:/Users/User/Documents/Wavis/wavis-public/target/debug/wavis-gui.exe',
    },
  },
});

console.log('Handles:', await browser.getWindowHandles());
console.log('URL:', await browser.getUrl());
console.log('outerHTML:', await browser.execute(() => document.documentElement.outerHTML));

await browser.deleteSession();
