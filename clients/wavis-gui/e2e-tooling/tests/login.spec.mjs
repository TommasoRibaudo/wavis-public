// Live-backend: exercises the real Login and DeviceSetup UIs end to end —
// the manual "new device" Wavis-ID entry path, the trusted-device
// password-only path, and (closed-alpha #266/#269) full UI-driven
// registration through the invite-gated DeviceSetup form.
//
// The first test uses an account created out-of-band via REST
// (registerDevice(), which sends ALPHA_INVITE_CODE) so it can focus purely
// on the login flows. The second test drives DeviceSetup's registration
// form itself, invite-code field included, via registerViaUi — see
// README.md for why REST-seeding stays the default for every *other*
// spec's setup (this is the one place UI-driven registration is the thing
// under test).
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
  registerViaUi,
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

test('a closed-alpha account registered through the real DeviceSetup UI can log back in', async ({
  app,
}) => {
  await waitForBackendHealth();

  const inviteCode = process.env.ALPHA_INVITE_CODE;
  if (!inviteCode) throw new Error('ALPHA_INVITE_CODE must be set for this spec');

  const main = app.page();
  await leaveRoomIfActive(main);
  // /setup is a top-level route reachable via direct navigation regardless
  // of auth state (DeviceSetup sits outside AuthGate — see routes.ts), the
  // same property loginViaUi relies on for /login. No prior-session
  // logout dance needed.
  await main.goto(new URL('/setup', main.url()).href);

  const suffix = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  const password = `e2e-register-ui-${suffix}`;
  const { recoveryId } = await registerViaUi(main, {
    username: `E2E-Register-${suffix}`,
    password,
    inviteCode,
    serverUrl: SERVER_URL,
  });
  expect(recoveryId).toMatch(/^wvs-/);

  // DeviceSetup's step 4 is the (unrelated, #67) "launch on startup"
  // onboarding prompt every fresh registration lands on — skip it to reach
  // the authenticated app and prove the session it produced is real.
  await main.getByText('/skip', { exact: true }).click();
  await expect(main).not.toHaveURL(/\/setup/);
  await expect(main.getByText('/settings', { exact: true }).first()).toBeVisible();

  // Log out and log back in with the recovery ID + password DeviceSetup
  // just produced — confirms the invite-gated registration created a real,
  // durable account, not just a UI-only success state.
  await main.getByText('/settings', { exact: true }).first().click();
  await main.getByText('/logout — sign out of this device', { exact: true }).click();
  await expect(main).toHaveURL(/\/login/);

  await loginViaUi(main, { recoveryId, password, serverUrl: SERVER_URL });
  await expect(main).not.toHaveURL(/\/login/);
  await expect(main.getByText('/settings', { exact: true }).first()).toBeVisible();
});
