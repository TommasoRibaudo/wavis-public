// Live-backend: exercises the real Login UI end to end — the manual
// "new device" Wavis-ID entry path and the trusted-device password-only
// path — with an account created out-of-band via REST.
//
// The DeviceSetup registration UI is not exercised here: account creation
// goes through registerDevice() (REST, which sends ALPHA_INVITE_CODE) so
// this spec can focus on the login flows. DeviceSetup does have a working
// invite-code field now (closed-alpha #266) and registerViaUi in
// live-backend-helpers.mjs drives it — see README.md for why REST-seeding
// stays the default for setup.
//
// Requires a reachable backend (see README's "Live-backend specs" section)
// and a debug exe built with VITE_ALLOW_INSECURE_TLS=true /
// VITE_AUTH_STORE_NAME set, so it never touches a real persisted session on
// this machine (see driver.mjs's WAVIS_KEYRING_SERVICE).
import { test, expect } from './fixtures.mjs';
import {
  SERVER_URL,
  waitForBackendHealth,
  leaveRoomIfActive,
  registerDevice,
  loginViaUi,
} from './live-backend-helpers.mjs';

const UNAUTHENTICATED_PATHS = ['/login', '/setup', '/recover', '/pair'];

test('recovery ID logs in on the new-device path, then password-only on the trusted-device path', async ({
  app,
}) => {
  await waitForBackendHealth();

  const main = app.page();
  await leaveRoomIfActive(main);

  const identity = await registerDevice();

  // Log out any persisted session first so the login flows under test start
  // from a real logged-out state. (On /recover or /pair there is nothing to
  // log out of — loginViaUi navigates straight to /login from anywhere.)
  const pathname = new URL(main.url()).pathname;
  if (!UNAUTHENTICATED_PATHS.some((p) => pathname.startsWith(p))) {
    await main.getByText('/settings', { exact: true }).first().click();
    await main.getByText('/logout — sign out of this device', { exact: true }).click();
    await expect(main).toHaveURL(/\/login/);
  }

  // Manual "new device" path: Wavis ID + password + server URL.
  await loginViaUi(main, {
    recoveryId: identity.recovery_id,
    password: identity.phrase,
    serverUrl: SERVER_URL,
  });
  await expect(main).not.toHaveURL(/\/login/);
  await expect(main.getByText('/settings', { exact: true }).first()).toBeVisible();

  // Log out -> lands on /login in trusted mode (recovery ID preserved by
  // logout()), which shows only a password field.
  await main.getByText('/settings', { exact: true }).first().click();
  await main.getByText('/logout — sign out of this device', { exact: true }).click();
  await expect(main).toHaveURL(/\/login/);
  await expect(main.getByLabel('Wavis ID', { exact: true })).toHaveCount(0);

  // Password-only trusted-device login with the same account.
  await main.getByLabel('Password', { exact: true }).fill(identity.phrase);
  await main.getByText('/login', { exact: true }).click();
  await expect(main).not.toHaveURL(/\/login/);
  await expect(main.getByText('/settings', { exact: true }).first()).toBeVisible();
});
