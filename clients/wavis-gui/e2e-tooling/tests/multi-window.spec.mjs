// Backend-independent: confirms the core multi-window contract this harness
// is built on — every open Tauri window is its own independent Playwright
// Page in context.pages(), keyed by URL path, with its own DOM. The
// diagnostics window is used as the vehicle since it's always open in this
// debug build (VITE_DIAGNOSTICS=true) and needs no prior click to appear.
import { test, expect } from './fixtures.mjs';

test('main and diagnostics windows are independent pages', async ({ app }) => {
  const pages = app.pages();
  expect(pages.length).toBeGreaterThanOrEqual(2);

  const main = app.page();
  const diagnostics = app.getPageByPath('/diagnostics');
  expect(main).not.toBe(diagnostics);
  expect(diagnostics.url()).not.toBe(main.url());

  // Each page reads its own DOM independently — not a stale/shared snapshot.
  const diagnosticsBodyText = await diagnostics.locator('body').innerText();
  expect(diagnosticsBodyText.trim().length).toBeGreaterThan(0);
});
