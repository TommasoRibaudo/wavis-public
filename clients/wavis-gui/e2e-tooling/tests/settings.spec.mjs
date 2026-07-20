// The AuthGate status bar's "/settings" link (src/features/auth/AuthGate.tsx)
// is only rendered for an authenticated session, and is hidden while in an
// active room (see AuthGate's `isRoomPage` check) — there's a second,
// unrelated "/settings" toggle inside ActiveRoom.tsx that opens an inline
// panel instead of navigating, and requires a live backend/active room to
// reach at all. This spec exercises only the AuthGate one, and skips rather
// than assumes when the persisted session on this machine doesn't put it in
// reach — see README's "Reality check" on why session state isn't
// guaranteed.
import { test, expect } from './fixtures.mjs';

const UNAUTHENTICATED_PATHS = ['/login', '/setup', '/recover', '/pair'];

test('/settings navigates to the settings page and back', async ({ app, skip }) => {
  const main = await app.page();
  const { pathname } = new URL(await main.url());

  skip(
    UNAUTHENTICATED_PATHS.some((p) => pathname.startsWith(p)),
    'No authenticated session persisted on this machine — the AuthGate status bar (and its /settings link) never renders on /login, /setup, /recover, /pair.',
  );
  skip(
    pathname.startsWith('/room'),
    'Main window is in an active room — AuthGate hides its status bar there; the in-room "/settings" toggle (ActiveRoom.tsx) is a different, live-backend-only affordance.',
  );

  await main.getByText('/settings', { exact: true }).first().click();
  await expect(main).toHaveURL(/\/settings/);
  await expect(main.getByRole('heading', { name: 'settings' })).toBeVisible();

  await main.getByText('← /channels', { exact: true }).click();
  await expect(main).not.toHaveURL(/\/settings/);
});
