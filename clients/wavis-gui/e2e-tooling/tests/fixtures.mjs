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
});

export { expect };
