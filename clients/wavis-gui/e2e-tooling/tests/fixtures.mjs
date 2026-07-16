// Shared Playwright Test fixture for driving the real Wavis app. Every spec
// should import `test`/`expect` from here, not from '@playwright/test'
// directly, so the app launch/close lifecycle is never duplicated per-file.
import { test as base, expect } from '@playwright/test';
import { launchApp } from '../driver.mjs';

export const test = base.extend({
  app: async ({}, use) => {
    const app = await launchApp();
    await use(app);
    await app.close();
  },
  // A SECOND real, independently-driveable app instance — lazy (only
  // launched by specs that destructure it, e.g. two-instances.spec.mjs), so
  // it costs nothing to every other spec. Distinct CDP port, auth store
  // file, and keychain service from `app` — see driver.mjs's launchApp
  // options and README.md's "Two simultaneous GUI instances" section.
  appB: async ({}, use) => {
    const app = await launchApp({
      port: 9223,
      authStoreName: 'wavis-auth-e2e-b.json',
      keyringService: 'com.wavis.gui.e2e-b',
    });
    await use(app);
    await app.close();
  },
});

export { expect };
