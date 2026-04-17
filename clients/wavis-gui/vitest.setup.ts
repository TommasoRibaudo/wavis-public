/**
 * Vitest setup file — polyfills for APIs available in Tauri webview
 * but missing in Node.js test runners.
 */

// atob / btoa polyfills — used by parseJwtExpiry in lib/auth.ts
if (typeof globalThis.atob === 'undefined') {
  globalThis.atob = (str: string): string =>
    Buffer.from(str, 'base64').toString('binary');
}

if (typeof globalThis.btoa === 'undefined') {
  globalThis.btoa = (str: string): string =>
    Buffer.from(str, 'binary').toString('base64');
}
