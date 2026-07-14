// Live-backend: exercises the real auth UI end to end — DeviceSetup
// registration, logout, and Login's recovery-ID-based re-login (both the
// "trusted device" password-only path and the manual "new device" Wavis ID
// entry path). This is the ONLY spec that drives the real auth UI from
// scratch; room-join/chat/participants use registerViaUi (a thinner version
// of the same flow) purely as setup, not as something under test.
//
// Requires a reachable backend (see README's "Live-backend specs" section)
// and a debug exe built with VITE_ALLOW_INSECURE_TLS=true /
// VITE_AUTH_STORE_NAME set, so it never touches a real persisted session on
// this machine (see driver.mjs's WAVIS_KEYRING_SERVICE).
//
// Each run registers a fresh throwaway account (timestamp-suffixed
// username) — registration has no email/CAPTCHA step, so this is cheap and
// deterministic, at the cost of leaving throwaway accounts in the backend's
// database (acceptable for a disposable local docker-compose instance).
import { test, expect } from './fixtures.mjs';
import { SERVER_URL, waitForBackendHealth, leaveRoomIfActive } from './live-backend-helpers.mjs';

const UNAUTHENTICATED_PATHS = ['/login', '/setup', '/recover', '/pair'];

async function currentPathname(page) {
  return new URL(page.url()).pathname;
}

/** Navigates from wherever the app currently is to the /setup registration form. */
async function getToSetup(main) {
  let pathname = await currentPathname(main);

  if (!UNAUTHENTICATED_PATHS.some((p) => pathname.startsWith(p))) {
    // Authenticated somewhere (/, /channels, /settings, ...) — log out first.
    await main.getByText('/settings', { exact: true }).first().click();
    await main.getByText('/logout — sign out of this device', { exact: true }).click();
    await expect(main).toHaveURL(/\/login/);
    pathname = await currentPathname(main);
  }

  test.skip(
    pathname.startsWith('/recover') || pathname.startsWith('/pair'),
    'App landed on /recover or /pair, not /login or /setup — no scripted path from here without assuming unrelated in-progress state.',
  );

  if (pathname.startsWith('/login')) {
    // Trusted-device mode shows only a password field; reveal the /setup
    // link by declaring "not you" first (also required to reach /setup at
    // all) — unless already in new-device mode, where it doesn't exist.
    // Login.tsx renders "loading..." until its async store reads resolve, so
    // an instant .isVisible() snapshot here would race that — use a real
    // click with Playwright's built-in auto-wait/retry instead, and only
    // treat a genuine absence (not-yet-rendered doesn't count) as "already
    // in new-device mode".
    try {
      await main
        .getByText('Not you / log in on a new device', { exact: true })
        .click({ timeout: 10_000 });
    } catch {
      // Already in new-device mode — /setup should be directly reachable.
    }
    await main.getByText('/setup', { exact: true }).click();
    await expect(main).toHaveURL(/\/setup/);
  }
}

test('register creates an account, and recovery ID logs back in on both trusted and new-device paths', async ({
  app,
}) => {
  await waitForBackendHealth();

  const main = app.page();
  await leaveRoomIfActive(main);

  await getToSetup(main);

  const suffix = Date.now().toString(36);
  const username = `e2e-login-${suffix}`;
  const password = `e2e-password-${suffix}`;

  await main.getByLabel('Username', { exact: true }).fill(username);
  await main.getByLabel('Password', { exact: true }).fill(password);
  await main.getByLabel('Confirm Password', { exact: true }).fill(password);
  await main.getByText('/next', { exact: true }).click();

  await main.getByLabel('Server URL', { exact: true }).fill(SERVER_URL);
  if (SERVER_URL.startsWith('http://')) {
    await main.getByText('--danger-insecure-tls', { exact: true }).click();
  }
  await main.getByText('/register', { exact: true }).click();

  const recoveryIdLocator = main.locator('code').first();
  await expect(recoveryIdLocator).toBeVisible({ timeout: 15_000 });
  const recoveryId = (await recoveryIdLocator.innerText()).trim();
  expect(recoveryId.length).toBeGreaterThan(0);

  await main.getByText('I have saved my Recovery ID (required)', { exact: false }).click();
  await main.getByText('/continue', { exact: true }).click();

  // Authenticated: AuthGate's /settings link should now be reachable.
  await expect(main).not.toHaveURL(/\/setup/);
  await expect(main.getByText('/settings', { exact: true }).first()).toBeVisible();

  // Log out -> lands on /login in trusted mode (recovery ID preserved by logout()).
  await main.getByText('/settings', { exact: true }).first().click();
  await main.getByText('/logout — sign out of this device', { exact: true }).click();
  await expect(main).toHaveURL(/\/login/);
  await expect(main.getByLabel('Wavis ID', { exact: true })).toHaveCount(0);

  // Password-only trusted-device login with the just-created account.
  await main.getByLabel('Password', { exact: true }).fill(password);
  await main.getByText('/login', { exact: true }).click();
  await expect(main).not.toHaveURL(/\/login/);
  await expect(main.getByText('/settings', { exact: true }).first()).toBeVisible();

  // Log out again, then exercise the manual "new device" Wavis-ID entry path.
  await main.getByText('/settings', { exact: true }).first().click();
  await main.getByText('/logout — sign out of this device', { exact: true }).click();
  await expect(main).toHaveURL(/\/login/);

  await main.getByText('Not you / log in on a new device', { exact: true }).click();
  await expect(main.getByLabel('Wavis ID', { exact: true })).toBeVisible();

  await main.getByLabel('Wavis ID', { exact: true }).fill(recoveryId);
  await main.getByLabel('Password', { exact: true }).fill(password);
  await main.getByText('/login', { exact: true }).click();

  await expect(main).not.toHaveURL(/\/login/);
  await expect(main.getByText('/settings', { exact: true }).first()).toBeVisible();
});
