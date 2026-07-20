// Playwright-flavored `expect()` used by every spec, backed by
// expect-webdriverio for element-level assertions. Only the matcher names
// actually used across the 18 spec files are implemented — extend here if a
// spec needs another one.
import { expect as wdioExpect } from 'expect-webdriverio';
import { Locator } from './page-adapter.mjs';

async function pollUntil(fn, { timeout = 5000, interval = 200 } = {}) {
  const deadline = Date.now() + timeout;
  for (;;) {
    try {
      return await fn();
    } catch (err) {
      if (Date.now() > deadline) throw err;
      await new Promise((resolve) => setTimeout(resolve, interval));
    }
  }
}

function isPage(value) {
  return value && typeof value.url === 'function' && typeof value._focus === 'function';
}

function pageMatchers(page) {
  const assertURL = (matcher, negate, opts) =>
    pollUntil(async () => {
      const url = await page.url();
      const pass = matcher instanceof RegExp ? matcher.test(url) : url.includes(matcher);
      if (pass === negate) {
        throw new Error(
          `expect(page)${negate ? '.not' : ''}.toHaveURL(${matcher}) failed — actual url: ${url}`,
        );
      }
    }, opts);

  // Page objects also flow through plain Jest-style assertions in a couple
  // of specs (e.g. expect(main).toBeTruthy()) — start from the base
  // wdioExpect() result (which includes the full Jest matcher set) and layer
  // the page-specific toHaveURL on top.
  const base = wdioExpect(page);
  base.toHaveURL = (matcher, opts) => assertURL(matcher, false, opts);
  base.not.toHaveURL = (matcher, opts) => assertURL(matcher, true, opts);
  return base;
}

// Playwright's expect(locator).toX() re-queries the live DOM and retries
// until it passes (or its timeout elapses) — that's the whole point of
// asserting on a Locator instead of a resolved value. Each check below used
// to resolve the locator ONCE and hand that single snapshot to
// expect-webdriverio, which has no way to see a later DOM change no matter
// how it's configured. Wrapped in pollUntil() so every attempt re-resolves
// fresh (`wait: 0` disables expect-webdriverio's own inner retry, since
// pollUntil already owns the retry loop — nesting both just double-waits).
function locatorMatchers(locator) {
  return {
    toBeVisible: (opts) =>
      pollUntil(
        async () => wdioExpect(await locator.resolveOne()).toBeDisplayed({ ...opts, wait: 0 }),
        opts,
      ),
    toHaveCount: (n, opts) =>
      pollUntil(
        async () =>
          wdioExpect(await locator.resolveAll()).toBeElementsArrayOfSize(n, { ...opts, wait: 0 }),
        opts,
      ),
    toHaveAttribute: (name, value, opts) =>
      pollUntil(
        async () =>
          wdioExpect(await locator.resolveOne()).toHaveAttribute(name, value, { ...opts, wait: 0 }),
        opts,
      ),
    toHaveText: (value, opts) =>
      pollUntil(
        async () => wdioExpect(await locator.resolveOne()).toHaveText(value, { ...opts, wait: 0 }),
        opts,
      ),
    not: {
      toBeVisible: (opts) =>
        pollUntil(
          async () =>
            wdioExpect(await locator.resolveOne()).not.toBeDisplayed({ ...opts, wait: 0 }),
          opts,
        ),
      toHaveCount: (n, opts) =>
        pollUntil(
          async () =>
            wdioExpect(await locator.resolveAll()).not.toBeElementsArrayOfSize(n, {
              ...opts,
              wait: 0,
            }),
          opts,
        ),
      toHaveAttribute: (name, value, opts) =>
        pollUntil(
          async () =>
            wdioExpect(await locator.resolveOne()).not.toHaveAttribute(name, value, {
              ...opts,
              wait: 0,
            }),
          opts,
        ),
      toHaveText: (value, opts) =>
        pollUntil(
          async () =>
            wdioExpect(await locator.resolveOne()).not.toHaveText(value, { ...opts, wait: 0 }),
          opts,
        ),
    },
  };
}

export function expect(received) {
  if (received instanceof Locator) return locatorMatchers(received);
  if (isPage(received)) return pageMatchers(received);
  // Plain values, WDIO elements/arrays, numbers, etc — delegate directly.
  return wdioExpect(received);
}

// Playwright's expect.poll(fn, {timeout, intervals, message}) — only the
// matchers actually chained onto it across the spec suite (.toBe,
// .toBeLessThan) are implemented. `intervals` (Playwright's array-of-ms
// retry cadence) is honored via its first entry as a flat poll interval — no
// spec here uses a backoff sequence, just a single "check every N ms" cadence.
expect.poll = (fn, { timeout = 5000, intervals, message } = {}) => {
  const opts = { timeout, interval: intervals?.[0] };
  return {
    toBe: (expected) =>
      pollUntil(async () => {
        const actual = await fn();
        if (actual !== expected) {
          throw new Error(
            message ??
              `expect.poll(): expected ${JSON.stringify(actual)} to be ${JSON.stringify(expected)}`,
          );
        }
      }, opts),
    toBeLessThan: (expected) =>
      pollUntil(async () => {
        const actual = await fn();
        if (!(actual < expected)) {
          throw new Error(
            message ??
              `expect.poll(): expected ${JSON.stringify(actual)} to be less than ${JSON.stringify(expected)}`,
          );
        }
      }, opts),
  };
};
