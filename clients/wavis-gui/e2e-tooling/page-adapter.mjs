// Thin Playwright-Page/Locator-shaped adapter over a single shared
// WebdriverIO `browser` session, so the 18 existing spec files translate
// mechanically (add `await`, swap a selector helper) instead of a full
// rewrite. See driver.mjs for how Page instances are constructed.
//
// WebdriverIO has exactly one "current window" per session — every method
// here re-switches to its own Page's window handle before touching the DOM,
// so interleaved use of multiple Page objects (multi-window specs) stays
// correct regardless of call order.

function cssAttrEscape(value) {
  return String(value).replace(/\\/g, '\\\\').replace(/"/g, '\\"');
}

// XPath 1.0 has no escape for a literal containing both quote types; every
// string this suite matches on avoids that combination, so concat() is a
// belt-and-suspenders fallback rather than something exercised in practice.
function xpathLiteral(text) {
  if (!text.includes('"')) return `"${text}"`;
  if (!text.includes("'")) return `'${text}'`;
  return `concat(${text
    .split('"')
    .map((part) => `"${part}"`)
    .join(`, '"', `)})`;
}

// Tag-agnostic text match, restricted to the innermost matching element(s) —
// the same "leaf, not container" behavior Playwright's getByText has, so a
// `<div>outer <span>TEXT</span></div>` matches the <span>, not the <div>.
function textXPath(text, exact) {
  const lit = xpathLiteral(text);
  const test = exact
    ? `normalize-space(string(.))=${lit}`
    : `contains(normalize-space(string(.)), ${lit})`;
  return `.//*[${test} and not(.//*[${test}])]`;
}

function stripVisible(selector) {
  if (typeof selector !== 'string') return { selector: '*', visibleOnly: false };
  if (selector.includes(':visible')) {
    return { selector: selector.replace(/:visible/g, '').trim() || '*', visibleOnly: true };
  }
  return { selector, visibleOnly: false };
}

const ROLE_SELECTORS = {
  heading: 'h1, h2, h3, h4, h5, h6, [role="heading"]',
  button: 'button, [role="button"]',
  img: 'svg, img, [role="img"]',
  option: '[role="option"], option',
  listbox: '[role="listbox"], select',
};

// Matches Playwright's getByRole `name` semantics closely enough for this
// codebase: an element's accessible name is its aria-label if present,
// otherwise its own text content (covers both patterns actually used here —
// icons expose name via aria-label, buttons/headings via visible text).
function accessibleNameFilter(name, { exact = false } = {}) {
  const matches = (candidate) => {
    if (name instanceof RegExp) return name.test(candidate);
    return exact ? candidate === name : candidate.includes(name);
  };
  return async (el) => {
    const ariaLabel = await el.getAttribute('aria-label').catch(() => null);
    if (ariaLabel != null && matches(ariaLabel)) return true;
    const text = await el.getText().catch(() => '');
    return matches(text);
  };
}

export class Locator {
  constructor(page, selector, { hasText, filter, parent, regexText } = {}) {
    this.page = page;
    const stripped = stripVisible(selector);
    this.selector = stripped.selector;
    this._visibleOnly = stripped.visibleOnly;
    this.hasText = hasText;
    this.filter = filter;
    this.parent = parent;
    this.onlyFirst = false;
    this._extraRawAll = null;
    // Set only by getByText(regex) — see #rawAll(). Matching happens
    // server-side via a single execute() call instead of resolving every
    // element on the page and round-tripping getText() on each (that N+1
    // pattern was observed to push a single lookup on a busy room page past
    // several seconds).
    this._regexText = regexText;
  }

  #clone() {
    const c = new Locator(this.page, this.selector, {
      hasText: this.hasText,
      filter: this.filter,
      parent: this.parent,
      regexText: this._regexText,
    });
    c._visibleOnly = this._visibleOnly;
    c.onlyFirst = this.onlyFirst;
    c._extraRawAll = this._extraRawAll;
    return c;
  }

  async #rawAll() {
    let base;
    if (this._regexText) {
      await this.page._focus();
      const scopeEl = this.parent ? await this.parent.resolveOne() : undefined;
      // The embedded WebDriver provider (tauri-plugin-wdio-webdriver) cannot
      // hand back DOM element references returned directly from an
      // execute() script — every element in the return value deserializes
      // to `null` (confirmed: even `execute(() => document.body)` comes
      // back null). So instead of returning matched elements, the script
      // tags them with a temporary attribute, and a normal, page-scoped
      // `$$()` selector query — proven to work, same mechanism every other
      // locator path here already relies on — fetches real, usable element
      // handles for that attribute afterward.
      const markerAttr = 'data-e2e-regex-match';
      await this.page.browser.execute(
        (root, source, flags, attr) => {
          const re = new RegExp(source, flags);
          const ownText = (el) => (el.textContent || '').replace(/\s+/g, ' ').trim();
          const scope = root || document;
          for (const stale of document.querySelectorAll(`[${attr}]`)) stale.removeAttribute(attr);
          // Single pass to collect every text-matching element, then keep
          // only the "leaves" (no other match nested inside them) via
          // pairwise el.contains() checks over just that match set — NOT a
          // fresh querySelectorAll('*') subtree re-scan per candidate. That
          // per-candidate re-scan is O(elementCount²): any ancestor whose
          // full textContent happens to contain the match (body, layout
          // wrappers, the whole sidebar, ...) re-walks its entire subtree,
          // and on a page with real accumulated state (many channels/rows)
          // this can measure multiple seconds per call. Matches are
          // typically single-digit count, so the pairwise check here is
          // cheap regardless of page size.
          const matches = [];
          for (const el of scope.querySelectorAll('*')) {
            const text = ownText(el);
            if (text && re.test(text)) matches.push(el);
          }
          const leaves = matches.filter(
            (el) => !matches.some((other) => other !== el && el.contains(other)),
          );
          leaves.forEach((el) => el.setAttribute(attr, ''));
        },
        scopeEl ?? null,
        this._regexText.source,
        this._regexText.flags,
        markerAttr,
      );
      base = await this.page.browser.$$(`[${markerAttr}]`);
    } else if (this.parent) {
      const parentEl = await this.parent.resolveOne();
      base = await parentEl.$$(this.selector);
    } else {
      await this.page._focus();
      base = await this.page.browser.$$(this.selector);
    }
    if (this._extraRawAll) return [...base, ...(await this._extraRawAll())];
    return base;
  }

  async #passes(el) {
    if (this._visibleOnly && !(await el.isDisplayed().catch(() => false))) return false;
    if (this.hasText) {
      const text = await el.getText().catch(() => '');
      const ok =
        this.hasText instanceof RegExp ? this.hasText.test(text) : text.includes(this.hasText);
      if (!ok) return false;
    }
    if (this.filter && !(await this.filter(el))) return false;
    return true;
  }

  /** Every currently-matching element (for count/array assertions). */
  async resolveAll() {
    const raw = await this.#rawAll();
    const out = [];
    for (const el of raw) {
      if (await this.#passes(el)) {
        out.push(el);
        if (this.onlyFirst) break;
      }
    }
    return out;
  }

  /** The first currently-matching element — may be a non-existent handle. */
  async resolveOne() {
    const [first] = await this.resolveAll();
    if (first) return first;
    // A selector guaranteed to resolve to a non-existent element handle
    // (isDisplayed()/isExisting() report false/absent on it) — needed for
    // regexText locators, whose real selector ('*') would otherwise return
    // the document's first element instead of "nothing matched".
    const nonExistentSelector = this._regexText ? '__e2e-adapter-no-match__' : this.selector;
    if (this.parent) {
      const parentEl = await this.parent.resolveOne();
      return parentEl.$(nonExistentSelector);
    }
    await this.page._focus();
    return this.page.browser.$(nonExistentSelector);
  }

  first() {
    const c = this.#clone();
    c.onlyFirst = true;
    return c;
  }

  /** Intersection: elements matching both this locator and `other`'s criteria. */
  and(other) {
    const c = this.#clone();
    const prevFilter = c.filter;
    c.filter = async (el) => {
      if (prevFilter && !(await prevFilter(el))) return false;
      // `other` is always a Locator — private-method access is class-scoped,
      // not instance-scoped, so this reaches across to its own #passes.
      return other.#passes(el);
    };
    return c;
  }

  /** Union: elements matching either this locator or `other`. */
  or(other) {
    const c = this.#clone();
    c._extraRawAll = () => other.resolveAll();
    return c;
  }

  locator(subSelector, opts) {
    return new Locator(this.page, subSelector, { ...opts, parent: this });
  }

  async count() {
    return (await this.resolveAll()).length;
  }

  async click({ timeout = 5_000, ...opts } = {}) {
    // Playwright's click() auto-waits for actionability (default ~30s, or
    // the given `timeout`) before acting — WDIO's element.click() has no
    // such option, so an explicit wait-then-click reproduces that. Defaults
    // to a short wait even when the caller passes no timeout: resolveOne()
    // is a single-shot snapshot, and a caller that just finished its own
    // "wait until present" loop (e.g. joinDefaultSubRoomViaUi) can still
    // race a button that flickers out for a moment right at that boundary —
    // observed causing a hard "element wasn't found" failure with no retry
    // at all. Callers that want a fast, deliberate failure (e.g. the "Not
    // you" detour link, tried opportunistically in a try/catch) pass an
    // explicit `{ timeout }` of their own, same as before.
    await this.waitFor({ state: 'visible', timeout });
    const el = await this.resolveOne();
    return el.click(opts);
  }

  async fill(text) {
    const el = await this.resolveOne();
    await el.clearValue().catch(() => {});
    return el.setValue(text);
  }

  async press(key) {
    // Sending a key is a browser-level WDIO command (there is no per-element
    // equivalent) — it goes to whatever element currently has focus, so this
    // relies on a prior action (fill/click) having focused the target first.
    await this.page._focus();
    return this.page.browser.keys(key);
  }

  async innerText() {
    const el = await this.resolveOne();
    return el.getText();
  }

  async getAttribute(name) {
    const el = await this.resolveOne();
    return el.getAttribute(name);
  }

  async isVisible() {
    const el = await this.resolveOne();
    return el.isDisplayed().catch(() => false);
  }

  async isExisting() {
    const el = await this.resolveOne();
    return el.isExisting();
  }

  async isChecked() {
    const el = await this.resolveOne();
    return el.isSelected().catch(() => false);
  }

  async waitFor({ state = 'visible', timeout = 5000 } = {}) {
    const deadline = Date.now() + timeout;
    for (;;) {
      const matches = await this.resolveAll();
      if (state === 'visible') {
        for (const el of matches) {
          if (await el.isDisplayed().catch(() => false)) return;
        }
      } else if (state === 'attached') {
        if (matches.length > 0) return;
      } else if (state === 'hidden' || state === 'detached') {
        if (matches.length === 0) return;
      }
      if (Date.now() > deadline) {
        throw new Error(
          `Locator.waitFor({ state: "${state}" }) timed out after ${timeout}ms (selector: ${this.selector})`,
        );
      }
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
  }

  getByText(text, opts) {
    return getByText.call(this, text, opts);
  }

  getByRole(role, opts) {
    return getByRole.call(this, role, opts);
  }

  getByLabel(text, opts) {
    return getByLabel.call(this, text, opts);
  }

  getByTitle(text) {
    return getByTitle.call(this, text);
  }
}

// Shared between Page and Locator (both expose `.locator(selector, opts)`
// with the same signature) — defined once, bound onto both prototypes below.
function getByText(text, { exact = false } = {}) {
  if (text instanceof RegExp) return this.locator('*', { regexText: text });
  return this.locator(textXPath(text, exact));
}

function getByRole(role, { name, exact = false } = {}) {
  const base = ROLE_SELECTORS[role] ?? `[role="${role}"]`;
  if (name === undefined) return this.locator(base);
  return this.locator(base, { filter: accessibleNameFilter(name, { exact }) });
}

function getByLabel(text, { exact = true } = {}) {
  return this.locator(
    exact ? `[aria-label="${cssAttrEscape(text)}"]` : `[aria-label*="${cssAttrEscape(text)}"]`,
  );
}

function getByTitle(text) {
  return this.locator(`[title="${cssAttrEscape(text)}"]`);
}

export class Page {
  constructor(browser, handle, focusFn) {
    this.browser = browser;
    this.handle = handle;
    this._focusImpl = focusFn;
  }

  async _focus() {
    await this._focusImpl(this.handle);
  }

  async url() {
    await this._focus();
    return this.browser.getUrl();
  }

  async goto(url) {
    await this._focus();
    return this.browser.url(url);
  }

  /** matcher: string substring, RegExp, or (url: URL) => boolean — matches Playwright's page.waitForURL(). */
  async waitForURL(matcher, { timeout = 10_000 } = {}) {
    const deadline = Date.now() + timeout;
    for (;;) {
      const current = await this.url();
      const pass =
        typeof matcher === 'function'
          ? matcher(new URL(current))
          : matcher instanceof RegExp
            ? matcher.test(current)
            : current.includes(matcher);
      if (pass) return;
      if (Date.now() > deadline) {
        throw new Error(`waitForURL() timed out after ${timeout}ms — last url: ${current}`);
      }
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
  }

  async evaluate(fn, ...args) {
    await this._focus();
    return this.browser.execute(fn, ...args);
  }

  async innerText(selector = 'body') {
    return this.locator(selector).innerText();
  }

  // Replaces Playwright's page.on('console')/page.on('pageerror') push-based
  // listeners — WebdriverIO has no direct equivalent, so this self-injects a
  // capture patch via evaluate() instead of relying on driver-specific
  // browser-log support. Call startCapturingConsole() before the actions
  // under test, then consoleLogs()/consoleErrors() to read what was
  // captured.
  async startCapturingConsole() {
    await this.evaluate(() => {
      if (window.__e2eConsolePatched) return;
      window.__e2eConsolePatched = true;
      window.__e2eConsoleLogs = [];
      for (const level of ['log', 'debug', 'info', 'warn', 'error']) {
        const orig = console[level].bind(console);
        console[level] = (...args) => {
          window.__e2eConsoleLogs.push({ level, text: args.map(String).join(' ') });
          orig(...args);
        };
      }
      window.addEventListener('error', (e) => {
        window.__e2eConsoleLogs.push({ level: 'error', text: e.message });
      });
    });
  }

  /** Alias — some specs only care about errors, this reads from the same capture. */
  async startCapturingConsoleErrors() {
    return this.startCapturingConsole();
  }

  async consoleLogs() {
    const entries = await this.evaluate(() => window.__e2eConsoleLogs || []);
    return entries.map((e) => e.text);
  }

  async consoleErrors() {
    const entries = await this.evaluate(() => window.__e2eConsoleLogs || []);
    return entries.filter((e) => e.level === 'error').map((e) => e.text);
  }

  // Replaces Playwright's page.on('request') network observation —
  // WebdriverIO has no request-interception primitive over plain WebDriver,
  // so this patches window.fetch instead (the app's own sound playback goes
  // through fetch(), see notification-sounds.ts).
  async startCapturingFetchRequests(urlSubstring) {
    await this.evaluate((substr) => {
      if (window.__e2eFetchPatched) return;
      window.__e2eFetchPatched = true;
      window.__e2eFetchRequests = [];
      const origFetch = window.fetch.bind(window);
      window.fetch = (...args) => {
        const url = typeof args[0] === 'string' ? args[0] : args[0]?.url;
        if (url && url.includes(substr)) window.__e2eFetchRequests.push(url);
        return origFetch(...args);
      };
    }, urlSubstring);
  }

  async fetchRequests() {
    return this.evaluate(() => window.__e2eFetchRequests || []);
  }

  locator(selector, opts) {
    return new Locator(this, selector, opts);
  }

  getByText(text, opts) {
    return getByText.call(this, text, opts);
  }

  getByRole(role, opts) {
    return getByRole.call(this, role, opts);
  }

  getByLabel(text, opts) {
    return getByLabel.call(this, text, opts);
  }

  getByTitle(text) {
    return getByTitle.call(this, text);
  }
}
