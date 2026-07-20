// Backend-independent: src/App.tsx renders <TitleBar /> for the main window
// on every non-chromeless route (login, setup, channels, room, settings...),
// so these assertions hold regardless of auth/session state.
//
// Read-only on purpose — clicking "Close" would kill the app mid-suite, and
// clicking "Minimize" would leave the window in a state later specs don't
// expect (this file's tests are the only ones that touch TitleBar).
import { test, expect } from './fixtures.mjs';

test('title bar renders with window controls', async ({ app }) => {
  const main = await app.page();

  await expect(main.getByText('wavis', { exact: true })).toBeVisible();
  await expect(main.getByLabel('Minimize', { exact: true })).toBeVisible();
  await expect(main.getByLabel('Maximize', { exact: true })).toBeVisible();
  await expect(main.getByLabel('Close', { exact: true })).toBeVisible();
});
