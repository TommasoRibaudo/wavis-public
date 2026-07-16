// Backend-independent: the app must launch to a rendered, non-blank state
// regardless of whatever auth/session state is already persisted on this
// machine (see README's "Reality check" — this debug build reuses real
// login/session state, not a blank first-run state).
import { test, expect } from './fixtures.mjs';

test('main window launches and renders content', async ({ app }) => {
  const main = app.page();
  expect(main).toBeTruthy();

  const bodyText = await main.locator('body').innerText();
  expect(bodyText.trim().length).toBeGreaterThan(0);
});

test('diagnostics window opens automatically alongside the main window', async ({ app }) => {
  // The debug build bakes in VITE_DIAGNOSTICS=true (see README), so this
  // window is always present without any prior click — a free second
  // window for exercising the multi-window path.
  const diagnostics = app.getPageByPath('/diagnostics');
  expect(diagnostics.url()).toContain('/diagnostics');
});
