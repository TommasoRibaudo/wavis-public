// Shared test fixture for driving the real Wavis app. Every spec should
// import `test`/`expect` from here, not construct the app launch itself, so
// the launch/close lifecycle is never duplicated per-file.
//
// Built on node:test rather than @playwright/test (removed per issue #297 —
// the whole point of the migration is retiring Playwright, not just its CDP
// connection) — `test(name, async ({ app }) => {...})` keeps the same call
// shape spec files already use, so spec bodies only need their Playwright
// locator/expect calls translated, not their test-declaration structure.
import { test as nodeTest } from 'node:test';
import { launchApp } from '../driver.mjs';

export { expect } from '../expect-adapter.mjs';

function lazyApp(options) {
  let real;
  const ensure = async () => {
    real ??= await launchApp(options);
    return real;
  };
  return {
    page: async (...args) => (await ensure()).page(...args),
    pages: async (...args) => (await ensure()).pages(...args),
    getPageByPath: async (...args) => (await ensure()).getPageByPath(...args),
    get browser() {
      if (!real)
        throw new Error('Access .browser only after using the app (e.g. via page()/pages()).');
      return real.browser;
    },
    close: async () => {
      if (real) await real.close();
    },
  };
}

// Default matches the old playwright.config.mjs `timeout: 30_000`. Specs
// that need more headroom pass it as a third argument instead of Playwright's
// mid-test `test.setTimeout(ms)` (node:test's timeout is fixed at
// declaration time, not adjustable from inside the test body).
const DEFAULT_TIMEOUT_MS = 30_000;

// Thrown by `skip()` purely to unwind the test body right at the call site
// (matching Playwright's test.skip(condition) control flow) — caught below
// and never surfaces as a failure. node:test's own t.skip() call is what
// actually marks the test outcome.
class SkipSignal extends Error {}

export function test(name, fn, { timeoutMs = DEFAULT_TIMEOUT_MS } = {}) {
  nodeTest(name, { timeout: timeoutMs }, async (t) => {
    const app = await launchApp();
    // A SECOND real, independently-driveable app instance — lazy (only
    // launched by specs that destructure it, e.g. two-instances.spec.mjs),
    // so it costs nothing to every other spec. Distinct WebDriver port, auth
    // store file, and keychain service from `app` — see driver.mjs's
    // launchApp options and README.md's "Two simultaneous GUI instances"
    // section.
    const appB = lazyApp({
      port: 4446,
      authStoreName: 'wavis-auth-e2e-b.json',
      keyringService: 'com.wavis.gui.e2e-b',
    });
    // Replaces Playwright's mid-body `test.skip(condition, reason)` —
    // spec files call `skip(condition, reason)` from the destructured
    // fixture object instead.
    const skip = (condition, reason) => {
      if (condition) {
        t.skip(reason);
        throw new SkipSignal();
      }
    };
    try {
      await fn({ app, appB, skip });
    } catch (err) {
      if (err instanceof SkipSignal) return;
      throw err;
    } finally {
      await appB.close();
      await app.close();
    }
  });
}
