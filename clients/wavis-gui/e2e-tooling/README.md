# Wavis GUI e2e-tooling

Dev-only harness for launching and driving the real Wavis desktop app
(Tauri 2.0) — not a test suite, not shipped with the app. Use it to click
through and verify a GUI change actually works instead of relying only on
`tsc`/`eslint`/`vitest`/production build.

This is deliberately **not** wired into `clients/wavis-gui/package.json` or
CI — it's a standalone tool with its own `package.json` so it never affects
the app's own dependency tree or lockfile.

## How this works

Drives the real app via **WebdriverIO's `@wdio/tauri-service`** (v1.2, in
standalone mode — `startWdioSession()`/`cleanupWdioSession()`, not the
session-per-worker model), which speaks the standard WebDriver protocol to
whichever webview engine Tauri uses on the current platform. This replaced
an earlier CDP-based mechanism (Playwright attached directly to WebView2's
Chrome DevTools Protocol port) that only worked on Windows — WebView2 is the
only Tauri webview engine that exposes CDP at all; WebKitGTK (Linux) and
WKWebView (macOS, out of scope here — see #298) don't.

**Windows — `embedded` driver provider.** `tauri-plugin-wdio-webdriver`
(feature-gated, see "Build the app to test against" below) runs an
in-process WebDriver server inside the app itself; `@wdio/tauri-service`
connects to it directly, no separate driver process needed.

**Linux — `external` driver provider.** `tauri-driver` proxies to
WebKitWebDriver (the `webkit2gtk-driver` apt package's `WebKitWebDriver`
binary), the standard, upstream-documented path for driving WebKitGTK. See
"Linux setup" below.

`driver.mjs` picks the provider automatically from `process.platform` — spec
files never need to know which one is active.

**Page/Locator API.** `page-adapter.mjs` is a from-scratch adapter shaped
like Playwright's `Page`/`Locator` (`getByText`/`getByRole`/`getByLabel`,
`.click()`/`.fill()`/`.waitFor()`, `.and()`/`.or()`/`.first()`) over
WebdriverIO's single shared `browser` session, so the spec files below read
almost exactly like they did before the migration — internals moved to
WebdriverIO, call-site intent didn't. `expect-adapter.mjs` does the same for
assertions, wrapping `expect-webdriverio` and adding Playwright-style
auto-retry (`expect(locator).toBeVisible()` etc. re-resolve the locator and
retry until they pass or time out — see "Assertions actually retry" below)
plus a hand-rolled `expect.poll()` matching Playwright's polling-assertion
API.

**Multi-window.** Each open Tauri window shows up as its own WebDriver
window handle (`browser.getWindowHandles()`/`switchToWindow()`) — the
migration's equivalent of Playwright's `context.pages()`. Confirmed
empirically: the embedded provider uses the Tauri window **label** itself
(`'main'`, `'diagnostics'`, ...) as the WebDriver handle, which `driver.mjs`
relies on to reliably find the main window — `getWindowHandles()`'s order is
not guaranteed to be stable run-to-run, so don't revert to picking
`pages()[0]`.

**Runner.** `node:test` (built in), not `@wdio/cli`/mocha — see "Formal test
suite" below for why.

**Out of scope for this harness:** `browser.tauri.switchWindow()`/
`listWindows()` (that's the separate `tauri-plugin-wdio` execute/mock
bridge, not `tauri-plugin-wdio-webdriver`) — plain WebDriver window handles
cover everything the spec suite needs.

## One-time setup (per machine)

`npm install` from this directory.

**Windows:** nothing else — the `embedded` driver provider needs no
separate driver binary.

**Linux:** see "Linux setup" below for the one extra system package.

## Build the app to test against

```powershell
cd clients/wavis-gui/e2e-tooling
npm run build:app
```

This wraps `npx tauri build --debug --features e2e-webdriver --config
src-tauri/tauri.e2e.conf.json --no-bundle` (`build-app.mjs`) and sets every
build-time env var the spec suite needs in one place — see "Live-backend
specs" below for what each one does and why a developer's own ambient `.env`
used to (and still can) supply them instead. **Re-run this after any source
change you want to verify**, same as the old `npx tauri build --debug`.
Produces `target/debug/wavis-gui.exe` (or `wavis-gui` on Linux) **at the
Cargo workspace root** — `clients/wavis-gui/src-tauri` is a workspace
member, so output lands in the workspace's shared `target/`, not under
`src-tauri/target/`. `driver.mjs` always launches whatever's currently on
disk at that path.

You'll see the command end with an error about `TAURI_SIGNING_PRIVATE_KEY`
("A public key has been found, but no private key") — that's the
updater-bundle (msi/nsis) signing step failing, which is expected and
harmless here: it happens _after_ the exe itself is already built
("Finished `dev` profile... Built application at: .../wavis-gui.exe"), and
this harness only needs the raw exe, not a signed installer bundle.

**Why a wrapper script instead of a raw `tauri build` invocation.** Tauri's
build script validates every `.json` file under `src-tauri/capabilities/`
against the plugin permission ACL compiled into the binary, **regardless of
whether that capability is in the active `app.security.capabilities`
allowlist** — allowlisting only affects runtime loading, not build-time
validation. `wdio-webdriver:default` (the permission
`tauri-plugin-wdio-webdriver` needs) only exists when the `e2e-webdriver`
Cargo feature is on, so if its capability file (`e2e-webdriver.json`) ever
sat in `capabilities/` for a **plain** `cargo check`/`cargo build` (no dev,
no CI, no Stop hook, no other developer touching this crate), that build
would hard-fail with `Permission wdio-webdriver:default not found`. So the
real capability content lives at
`src-tauri/e2e-webdriver-capability.template.json` — committed, inert,
outside `capabilities/` — and `build-app.mjs` copies it into
`capabilities/e2e-webdriver.json` (gitignored) only for the duration of this
one build, removing it again immediately after, success or failure. A plain
`cargo check -p wavis-gui` is clean before, during a normal dev loop, and
after running `build:app`.

**Note:** this debug build shares the same OS app-data identifier
(`com.wavis.desktop`) as any normal Wavis install on this machine unless you
override `VITE_AUTH_STORE_NAME`/`WAVIS_AUTH_STORE_NAME` (see "Live-backend
specs" — `build-app.mjs` already does this for you). It also talks to a
**real backend** — UI state reflects real connection status
(connecting/offline/joined), not canned data.

## Linux setup

One extra system package, then everything else is identical to Windows —
same `driver.mjs`, same spec files, `driverProvider` picked automatically.

```bash
sudo apt-get update && sudo apt-get install -y webkit2gtk-driver
```

This installs the `WebKitWebDriver` binary that `tauri-driver` (pulled in
automatically by `@wdio/tauri-service` as an `external`-provider dependency)
proxies to. No `tauri-plugin-wdio-webdriver`/`e2e-webdriver` Cargo feature
involved on this path — that plugin is Windows-`embedded`-only; Linux drives
the app from the outside via the standard WebKitGTK WebDriver server
instead, so a plain `npm run build:app` still works (the `e2e-webdriver`
Cargo feature and its capability file are simply inert/no-ops on this
platform) and a plain `cargo build` needs no special handling either.

Building `wavis-gui` itself on Linux needs the usual Tauri Linux system
dependencies (`build-essential`, `libgtk-3-dev`,
`libayatana-appindicator3-dev`, `librsvg2-dev`, `patchelf`,
`libwebkit2gtk-4.1-dev`) — see [Tauri's own Linux prerequisites
docs](https://tauri.app/start/prerequisites/) if a fresh machine is missing
one; this repo doesn't duplicate that list.

Live-backend specs need the same Docker Compose stack as Windows — this
repo's infra is already Linux-native, so `docker compose up -d --build`
works identically from inside WSL/a native Linux dev machine.

**Confirmed working on Linux (WSL2/Ubuntu 24.04 + WSLg), no real audio
decode involved:** `launch`, `title-bar`, `multi-window`, `settings`,
`login` (×2), `chat`, `leave-rejoin`, `participants`, `room-join` — all
pass cleanly, both standalone and batched together in one `node --test`
invocation.

**Known gap, unresolved: real-audio-decode specs fail on Linux.**
`audio-received` and `reconnect` (both assert on `window.__wavisVoiceStats`
rmsLevel derived from a genuinely decoded incoming WebRTC/Opus audio
track — see audio-received.spec.mjs's header comment) fail with `peer
rmsLevel never entered the decoded-audio band`. Ruled out: this is _not_
the `Linux Dependencies: 4 packages may be missing` diagnostic warning
(`libgtk-3-0`/`libasound2`/`libatk-bridge2.0-0`/`libcups2`) — those are a
false positive on Ubuntu 24.04 (renamed to `libgtk-3-0t64`/
`libasound2t64`/etc. for the 64-bit time_t transition; all four were
already installed under their real names, confirmed via `dpkg -l`, and
installing them explicitly by the new names made no difference). The
rmsLevel check is computed entirely client-side via a Web Audio
`AnalyserNode` on the decoded PCM — it doesn't depend on the OS audio
output path (WSLg's PulseAudio RDP bridge) at all, which points at
WebKitGTK's WebRTC audio _decode_ itself failing silently rather than an
OS-level audio routing problem. Leading suspect, not yet confirmed:
missing GStreamer plugin packages for Opus/WebRTC — this machine has
`gstreamer1.0-plugins-base`/`-good` but not `-bad`/`-libav`, and
`gst-inspect-1.0` (the tool to check for the `opusdec`/`webrtcbin`
elements directly) isn't installed either. Not yet tried: `camera`,
`zz-network-quality`, `screen-share*` (×3), `two-instances`,
`zz-disconnect-sound` — all plausibly hit the same gap (real media), but
none were individually confirmed in this pass; do that first before
assuming they're all blocked on the same root cause.

**Known gap, unresolved: full-suite runs cascade-fail from one hung spec.**
When a spec's `launchApp()` call hangs badly enough to hit its outer
`test.timeoutMs` (as the real-audio-decode specs above do), the next spec
in the same `node --test` invocation can inherit stale state and also
fail — confirmed directly: `chat`/`leave-rejoin`/`launch`/`login` all pass
cleanly run together as their own batch, but failed when run as part of
the full 18-file `npm run test` suite immediately after `audio-received`'s
own hang. Root cause not isolated (a stale WebKitWebDriver session, an
orphaned process, or something else) — the pragmatic workaround used to
verify this branch was running specs in batches that exclude the specs
known to hang. Fix the real-audio-decode gap above first; this may turn
out to be entirely downstream of it.

## Usage

```js
import { launchApp } from './driver.mjs';

const app = await launchApp(); // spawns, connects WebdriverIO, ~4s settle
try {
  console.log(await Promise.all((await app.pages()).map((p) => p.url())));
  // e.g. ['http://tauri.localhost/room', '.../diagnostics']

  const main = await app.page(); // main window, or app.getPageByPath('/watch-all') etc.
  await main.url(); // focuses the window before a raw browser.* call
  await main.browser.saveScreenshot('C:/path/to/scratchpad/out.png');

  // Real interaction — Playwright-flavored locator API from here:
  await main.getByText('/settings', { exact: true }).first().click();
  const text = await main.innerText('body');
} finally {
  await app.close(); // always — otherwise the app process + WebDriver session linger
}
```

Then **read the PNG and actually look at it** — a blank/error frame is a
failure to launch, not a pass. Write this as a throwaway script in
scratchpad, not a committed file.

**Locator tip learned the hard way:** Wavis's terminal-style UI repeats
short command labels (`/settings`, `/mute`, etc.) in multiple places — use
`.first()`/`.nth()` or a more specific locator when a locator resolves to
multiple matches, same as you would on any real app.

## Multi-window

Confirmed working directly: `app.pages()` shows every open Tauri window as
its own `Page`, keyed by URL path (`/room`, `/diagnostics`, `/watch-all`,
`/screen-share-*`, `/camera-popout`, `/share-picker`, `/share-indicator` —
see `src-tauri/capabilities/default.json` for the full window-label list;
the WebDriver-visible URL path is what `getPageByPath()` matches against).

The `diagnostics` window is a free, always-available second window for
exercising the multi-window path without scripting any prior click — it
auto-opens on launch because `build:app` bakes in `VITE_DIAGNOSTICS=true`
(build-time, bundles the window at all) and `driver.mjs`'s `launchApp()`
sets `WAVIS_DIAGNOSTICS_WINDOW=1` per-launch (runtime, auto-opens it).

## Formal test suite (`tests/`)

```powershell
npm run test
```

runs `tests/*.spec.mjs` via Node's built-in `node:test` runner
(`node global-setup.mjs && node --test --test-force-exit
--test-concurrency=1 tests/**/*.spec.mjs`) — **not** `@wdio/cli`/mocha and
not `@playwright/test`. `--test-concurrency=1` matters: the app launch is
stateful and pinned to a single default WebDriver port, so parallel workers
would collide (confirmed directly — running multiple spec files without
this flag intermittently fails with "Tauri app exited before the embedded
WebDriver server became ready" as two instances race for the same port; the
flag makes that disappear entirely). `global-setup.mjs` prebuilds the
`ws-sfu-test` peer binary once up front so its first on-demand `cargo run`
compile doesn't eat into whichever spec happens to use `spawnPeer()` first.

`--test-force-exit` matters for the real-media specs (`audio-received`,
`camera`, `zz-network-quality`, `screen-share*`, `two-instances`):
`@livekit/rtc-node`'s native binding leaves a handle open even after
`room.disconnect()` resolves and every track/source is closed, so the
`node --test` process hangs indefinitely after a spec has already passed
(confirmed directly — the test body prints `ok 1` and the assertions are
genuinely correct, but the process never returns to the shell). This flag
(Node 22+) force-exits once all tests finish instead of waiting for the
event loop to drain naturally.

Every spec imports `test`/`expect` from `tests/fixtures.mjs`, not
`node:test`/the adapters directly — the `app` fixture there wraps
`launchApp()`/`app.close()` so lifecycle is never duplicated per-file.
Two mechanical differences from the old Playwright-based specs, both
following from `node:test`'s API rather than a harness limitation:

- **`skip(condition, reason)`** replaces Playwright's mid-body
  `test.skip(condition, reason)` — passed via the destructured fixture
  object: `async ({ app, skip }) => { skip(someCondition, 'reason'); ... }`.
- **A third `test()` argument `{ timeoutMs }`** replaces Playwright's
  mid-body `test.setTimeout(ms)` — `node:test`'s timeout is fixed at
  declaration time, not adjustable mid-test:
  `test('name', async ({ app }) => {...}, { timeoutMs: 240_000 })`.

**Prerequisite:** build the debug exe first (`npm run build:app`, see
above) — `npm run test` launches whatever's currently on disk.

Backend-independent coverage: `launch.spec.mjs`, `title-bar.spec.mjs`,
`multi-window.spec.mjs`, `settings.spec.mjs`. None of these assume a
particular login/session state — `settings.spec.mjs` explicitly `skip()`s
with a stated reason if the persisted session on the machine doesn't put the
AuthGate `/settings` link in reach (unauth'd routes, or main window already
in an active room), rather than assuming one state and flaking on the other.

Live-backend coverage: `login.spec.mjs`, `room-join.spec.mjs`,
`chat.spec.mjs`, `participants.spec.mjs`, `audio-received.spec.mjs`,
`reconnect.spec.mjs`, `screen-share.spec.mjs`,
`screen-share-rejoin-badges.spec.mjs`,
`screen-share-audio-not-heard-until-opened.spec.mjs`, `camera.spec.mjs`,
`two-instances.spec.mjs`, `zz-network-quality.spec.mjs`,
`zz-disconnect-sound.spec.mjs` — see "Live-backend specs" below for the
extra build step and backend they require. These also `skip()` when the
main window is mid-call, for the same reason. `two-instances.spec.mjs`
additionally uses the `appB` fixture (a second real `launchApp()`) — see
"Two simultaneous GUI instances" below.

`clients/wavis-gui/vite.config.ts`'s vitest `test.exclude` explicitly
excludes `e2e-tooling/**` — without it, vitest's default `*.spec.*` glob
would try to collect these specs when `npm test` runs from
`clients/wavis-gui`.

### Assertions actually retry

`expect(locator).toBeVisible()`/`toHaveCount()`/`toHaveText()`/
`toHaveAttribute()` (and their `.not.` counterparts) re-resolve the locator
against the live DOM and retry until the assertion passes or its timeout
elapses — matching Playwright's real `expect()` semantics, where asserting
on a _locator_ (not an already-resolved value) is the whole point: it can
observe a later DOM change. An earlier version of this adapter resolved the
locator once and handed that single snapshot to `expect-webdriverio`, which
had no way to see a change after that snapshot no matter how it was
configured — surfaced by a real race (an assertion immediately following a
state-changing action, with no separate `waitFor()` in between, would
intermittently see the pre-change state). If you're writing a new
assertion, you don't need to think about this — it's just how `expect()`
behaves now — but don't reintroduce a single-shot `resolveOne()`/
`resolveAll()` handed directly to `expect-webdriverio` if you're touching
`expect-adapter.mjs`.

## Live-backend specs

`login.spec.mjs`, `room-join.spec.mjs`, `chat.spec.mjs`, and
`participants.spec.mjs` drive real auth/channel/chat/participant flows
against a real backend. Two things the backend-independent specs don't need:

**1. A reachable local backend.** Use the root docker-compose stack (see
`doc/QUICKSTART.md` §1) — a clean, disposable instance, not a shared
dev/staging environment:

```powershell
docker compose up -d --build
```

The specs themselves poll `/health` before running (`waitForBackendHealth`
in `tests/live-backend-helpers.mjs`), so a slow container start doesn't need
a manual wait. Tear down when done:

```powershell
docker compose down
```

**2. A debug exe built for live-backend use** — `npm run build:app` (see
"Build the app to test against" above) already sets everything this needs:

- `VITE_ALLOW_INSECURE_TLS=true` — the local backend serves plain HTTP
  (`http://localhost:3000`), and `DeviceSetup`/`Login` reject `http://`
  server URLs unless this is set (default `false`).
- `VITE_AUTH_STORE_NAME=wavis-auth-e2e.json` — points the Tauri auth store
  at a file distinct from `wavis-auth.json`, so registering/logging in
  during these specs never overwrites a real persisted session on this
  machine. (The other half of that isolation — the OS keychain refresh-token
  entry — is handled automatically: `launchApp()` defaults
  `keyringService` to `com.wavis.gui.e2e` for every launch. This build-time
  var is the baseline for a single instance; two-instance specs additionally
  override it per-launch at runtime — see "Two simultaneous GUI instances"
  below.)
- `VITE_ALLOW_SERVER_OVERRIDE=true` — release builds hide the Server URL
  field entirely and silently use `VITE_DEFAULT_SERVER_URL` (see issue #332);
  this flag brings the field back so `registerViaUi`/`loginViaUi` can keep
  filling it by its `"Server URL"` accessible label.

These used to require exporting three env vars by hand before a raw `npx
tauri build --debug` — `build-app.mjs` folds them in so `npm run build:app`
alone is always live-backend-capable; there's no separate "plain" vs
"live-backend" debug build anymore. (Historically a developer's own ambient
`clients/wavis-gui/.env` supplied these implicitly for their day-to-day
testing — that file is gitignored and **not present in a fresh `git
worktree add` checkout**, which is exactly the gap that once made a debug
build silently authenticate against the real production backend using a
real persisted admin session instead of the local stack. `build-app.mjs`
exists so this harness never depends on that ambient file being there.)

Each live-backend spec that needs a GUI-side authenticated identity reuses
the persisted e2e session when one exists, and otherwise gets one via
`registerAndLoginViaUi` in `tests/live-backend-helpers.mjs`: a fresh,
timestamp-suffixed throwaway account created over REST (`registerDevice()`,
which sends the required invite code), then authenticated through the real
`Login` UI. Registration through the real `DeviceSetup` UI is not possible
against a closed-alpha backend — see "Known gap" below. `login.spec.mjs`
exercises the Login UI itself as the thing under test (both the new-device
and trusted-device paths); the other specs treat authentication purely as
setup. Channel/invite seeding for `room-join`/`chat`/`participants` uses
REST (`POST /auth/register`, `POST /channels`,
`POST /channels/:id/invites`) for the channel _owner_, so that identity never
needs to touch the GUI at all. Closed-alpha registration requires
`ALPHA_INVITE_CODE` to point at a pre-seeded multi-use invite.

**Second participant (chat/participants specs).** These need a second
connected participant to assert anything meaningful. `spawnPeer()` drives
`ws-sfu-test` (`scripts/ws-sfu-test`) in its interactive JSON-line mode as a
scriptable second signaling participant — chosen over `wavis-client`'s REPL
because `wavis-client` has no chat send/receive support at all (checked its
command parser — `create`/`join`/`invite`/`revoke`/`leave`/`status`/`name`/
`volume`/`help`/`quit` only). These specs verify the GUI's own rendering
reacts correctly to a second real participant — they do not verify a second
real GUI instance's own rendering. For that, see "Two simultaneous GUI
instances" below (`two-instances.spec.mjs`); `spawnPeer()` stays the
lighter-weight choice here since these specs don't need a second real
window, just a second real signaling participant.

### Two simultaneous GUI instances

`two-instances.spec.mjs` drives **two real, independently-controlled**
`launchApp()` instances at once against the same local backend — proving two
different accounts can occupy the same voice room simultaneously with no
"Session taken over by another device" error, and (a separate test)
documenting that the SAME account joining the same room a second time
genuinely IS displaced (server design, not a harness bug — see
`voice_orchestrator.rs`'s `evict_stale_session`, which evicts by account
`user_id`, not device or window).

**What isolates the two instances**, both already true of any single
`launchApp()` call plus two new per-launch options:

- A fresh temp profile dir per launch (the WebDriver-provider equivalent of
  `WEBVIEW2_USER_DATA_FOLDER`), so two processes never share a webview
  profile.
- `port` — give the second instance its own WebDriver port (`4446`; `4445`
  stays the default for `port`-less calls).
- `authStoreName` — without this, both instances share one Tauri store file
  (`wavis-auth.json`/`wavis-auth-e2e.json`, whichever the exe was built
  with) and race last-writer-wins on login state. Passing a distinct name
  makes `launchApp()` set `WAVIS_AUTH_STORE_NAME` per-process, which
  `auth.ts`'s `resolveStoreName()` reads at runtime via a Tauri command
  (`get_auth_store_name` in `src-tauri/src/main.rs`) — it wins over the
  build-time `VITE_AUTH_STORE_NAME` baked into the exe.
- `keyringService` — same problem, one level down: without this, both
  instances share one OS keychain refresh-token entry and a token rotation
  in one can invalidate the other mid-test. `keyring_service()` in
  `main.rs` already reads `WAVIS_KEYRING_SERVICE` at runtime; `launchApp()`
  exposes it as a per-call option instead of hardcoding one value for every
  launch.

`wavis-settings.json` (audio/video prefs) stays **shared on purpose** — it's
prefs-only, not identity, and isolating it wasn't needed for anything this
spec asserts.

**The account rule.** Two different accounts in the same voice channel never
displace each other. The SAME account joining the same voice channel twice
(regardless of device/window) always does — that's the server's intended
"ghost duplicate" prevention, not something to route around. A two-instance
spec that wants no displacement needs two REAL DIFFERENT accounts, one per
instance.

**Known gap, unresolved: `two-instances.spec.mjs`'s first test is flaky/failing
on Windows under the embedded provider.** The second `launchApp()` call
(`appB`, via `other = await appB.page()`) intermittently fails with
`Tauri app exited before the embedded WebDriver server became ready
(code=0, signal=null)` — confirmed via `captureBackendLogs: true` that the
app produces no stderr/panic before exiting, ruling out a Rust-side crash.
Log evidence points at `@wdio/tauri-service`'s embedded-provider session
handling rather than the app itself: the second launch's WebDriver traffic
reuses the _same session ID_ as the first instance's, and the first
instance's driver process gets torn down as a side effect of starting the
second — i.e. the two `startWdioSession()` calls aren't as independent as
`driver.mjs`'s per-launch `port`/`authStoreName`/`keyringService` isolation
assumes. Ruled out: `tauri-plugin-single-instance` (main.rs only registers
it for launches with no `WAVIS_AUTH_STORE_NAME` override — forcing an
explicit override on the primary `app` fixture didn't fix it either). This
predates the WebdriverIO migration in the sense that it was never confirmed
passing under Playwright either — round 1/2 of #297 both listed it as
never-verified, not a regression. The second test in the same file (same-
account displacement, single instance only) passes reliably. Needs either a
`@wdio/tauri-service` version bump/upstream fix, or restructuring
`launchApp()`'s embedded-provider path to avoid whatever global/shared state
`startWdioSession()` is keying off — not yet investigated further.

Ad-hoc snippet (outside the formal suite, e.g. a scratchpad script):

```js
import { launchApp } from './driver.mjs';

const a = await launchApp();
const b = await launchApp({
  port: 4446,
  authStoreName: 'wavis-auth-e2e-b.json',
  keyringService: 'com.wavis.gui.e2e-b',
});
try {
  // drive a.page() and b.page() independently — two real windows, two real accounts
} finally {
  await a.close();
  await b.close();
}
```

**Formerly a known gap, fixed:** `DeviceSetup` now has an invite-code field
(closed-alpha #266) that `registerViaUi` fills from a required `inviteCode`
argument, so UI-driven registration can succeed against a backend that
enforces closed-alpha invites (`routes.rs`'s `register` handler 401s without
one). No spec uses `registerViaUi` yet — every live-backend spec still
authenticates via `registerAndLoginViaUi`: `registerDevice()` (REST, already
sends `inviteCode`) creates the account, then `loginViaUi` authenticates it
through the real `Login` UI (`/login` is a top-level route reachable via
direct navigation even on a device that's never registered locally — see
`routes.ts`). That's a deliberate choice, not a limitation: REST-seeded setup
keeps specs whose subject is something else (chat, screen-share, ...) from
paying the cost of driving the full registration form, and avoids the
cascade-failure history below recurring for suite-wide runs. `login.spec.mjs`
is the one spec that exercises real auth UI as the thing under test — it
could be extended to cover `registerViaUi`'s new-device registration path
alongside its existing Login-UI coverage. Before REST-seeding was suite-wide,
full sequential runs cascade-failed: `login.spec.mjs` logged out the
persisted session, its UI registration 401'd, and every later spec that fell
back to `registerViaUi` died in setup — while the same specs passed in
isolation by silently riding the persisted session.

Run once the backend is up and the live-backend exe is built:

```powershell
npm run test
```

**Sub-rooms vs. the call itself.** `participants.spec.mjs` needs to know
this: being connected to a channel's voice call and being in a "synchronized
room" (sub-room) are distinct states (`joinedSubRoomId` can be `null` while
fully connected — see `frontend_voice_room.md`). `ParticipantsPanel` only
ever renders participants _inside_ their joined sub-room, so nothing shows
up there — no count, no rows — until an explicit `/join` on that room's row
(`joinDefaultSubRoomViaUi` for the GUI, `joinDefaultSubRoomAsPeer` sending
`join_sub_room` for the peer). Chat is channel-wide and needs none of this.

**Media-plane specs** (`audio-received.spec.mjs`, `screen-share.spec.mjs`,
`camera.spec.mjs`) additionally need:

- `@livekit/rtc-node` (in this package's own `package.json`, installed by the
  `npm install` above) — a real LiveKit Node SDK with a prebuilt native
  binary, used only by `livekit-tone-peer.mjs` to publish a real audio track.
  Neither `ws-sfu-test` nor `wavis-cli-test` can play this role:
  `wavis-cli-test`'s `start_share` is a bare signaling message with no
  pixels/audio ever attached, and it can't `auth`/`join_voice` into a GUI
  channel's voice call at all (it only joins standalone rooms).
- `window.__wavisVoiceStats` — a small `VITE_DIAGNOSTICS`-gated hook in
  `App.tsx` (same gate as the diagnostics window) that exposes the live
  per-participant `rmsLevel`/`isSpeaking` snapshot to page-context
  `evaluate()` calls. Needed because there's no Tauri-global bridge exposed
  to read the `diagnostics:voice-stats` Tauri event directly from a
  WebDriver-driven page. **Production-absent**: only assigned inside the
  existing diagnostics interval, gone in real release builds.
- The `--use-fake-ui-for-media-stream` launch flag (`driver.mjs`'s
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, Windows-specific — see below for
  what it does and why it's scoped to e2e launches only).

`audio-received.spec.mjs` is the load-bearing proof that real WebRTC media
works in this harness's debug exe (see "Known local media blocker" below
for the two fixes this needed): `livekit-tone-peer.mjs` connects
directly to LiveKit (reusing the `media_token` the backend hands a
`ws-sfu-test` peer on `join_voice` — see `waitForMediaToken` below) and
publishes a real 440Hz sine as 10ms Int16 PCM frames. The spec's primary
assertion polls `window.__wavisVoiceStats` for the peer's `rmsLevel` landing
in `0.2 < rmsLevel < 0.95` — a band chosen to exclude the two known
signaling-path values that also set `rmsLevel` and would otherwise produce a
false positive: LiveKit's `ActiveSpeakersChanged` hardcode of `1.0`, and the
boosted value of `RMS_START_THRESHOLD + 0.05 = 0.11` (both in
`livekit-media.ts`/`voice-room.ts`). `isSpeaking`/"Remote Speaking" in the
diagnostics window are corroboration only, not the proof, since server
active-speaker events can set those independent of real decoded audio.

**Not covered / follow-ups from the media-plane work:**

- Reverse audio direction (GUI mic → peer hears) — GUI mic capture is
  nondeterministic real hardware. `livekit-tone-peer.mjs`'s `Room` could
  gain a subscribe-and-measure mode as a follow-up.
- ~~Real watched pixels for screen share~~ — **done**: `livekit-tone-peer.mjs`'s
  `startVideoPattern()`/`stopVideoPattern()` publish a real moving RGBA video
  track under `TrackSource.SOURCE_SCREENSHARE` via `@livekit/rtc-node`'s
  `VideoSource`/`VideoFrame`, exercised by `zz-network-quality.spec.mjs` (see
  "Media quality + network simulation" below). A remote camera tile is still
  not covered — `LocalVideoTrack` would need to publish under
  `TrackSource.SOURCE_CAMERA` instead, as a separate follow-up.
- `screen-share.spec.mjs`'s Direction B (peer signals `start_share` with no
  media) intentionally only asserts the "waiting for stream..." indicator —
  there is no track to watch, by construction.

**Known local media blocker — RESOLVED.** Real WebRTC media over the
default local `docker compose up` stack initially failed for every spec
that needs it. Real diagnosis, not speculation (a throwaway
console-capture + LiveKit-server-log capture was used to trace it), needed
two fixes together:

1. The GUI's LiveKit connection failed ~13s after connecting:
   `[wavis:livekit-media] SFU connection failed: ConnectionError: could not
establish pc connection`. LiveKit's own server logs (`docker logs
wavis-livekit`) showed why: `deploy/livekit.yaml` had `use_external_ip:
true`, making LiveKit advertise a STUN-discovered **public** IP as its
   own ICE host candidate — unreachable from a client on the same machine.
   **Fix**: `use_external_ip: false` + an explicit `node_ip: 127.0.0.1`
   (this repo's LiveKit and its clients always run on the same host for
   local dev). A `@livekit/rtc-node` peer (`livekit-tone-peer.mjs`,
   non-browser) confirmed connecting successfully after this fix alone —
   proving real media over Docker's published ports works for a
   non-browser client.
2. The **browser** (WebView2/Chromium) client still failed after that fix.
   LiveKit's logs showed its only usable candidate from the browser was a
   STUN-discovered public IP (again unreachable against `node_ip:
127.0.0.1`) — its real local-network host candidate arrived as an
   unresolved `.local` mDNS hostname, because Chromium hides real local IPs
   behind randomly-generated mDNS names in ICE candidates by default (a
   privacy feature, on since ~Chrome 81). `rtc.use_mdns: true` (a real
   LiveKit/Pion config key, found via `strings` on the server binary — no
   mention in the shipped example config) was tried and did **not** fix it:
   mDNS resolution needs UDP multicast (`224.0.0.251:5353`), and Docker
   Desktop for Windows's bridge network doesn't forward multicast between
   the container and the host. **Fix**: `driver.mjs` now launches the debug
   exe with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` including
   `--disable-features=WebRtcHideLocalIpsWithMdns`, disabling the mDNS
   obfuscation entirely for e2e-launched processes only (never in
   `tauri.conf.json` — production/normal dev keeps the default privacy
   behavior). With real local IPs in its candidates, the browser connects
   normally.

**Current state**: `deploy/livekit.yaml` keeps `use_external_ip: false` +
`node_ip: 127.0.0.1` (both required) + `use_mdns: true` (harmless, no
longer load-bearing now that the driver-side fix means the browser never
sends an mDNS candidate in the first place). `audio-received.spec.mjs`
passes in full. Verify with `node --test tests/audio-received.spec.mjs`
(from this directory) — must connect and read a real decoded `rmsLevel`
within 20s.

**Three findings surfaced once media started working — all resolved:**

- `audio-received.spec.mjs`'s negative control (rmsLevel must decay to
  silence after `stopTone()`) was a real **product bug**, not a test bug:
  remote `rmsLevel` had no downward write path. `livekit-media.ts`'s
  analyser poll only emitted updates when it saw actual signal (RMS >
  0.001), so genuinely silent decoded audio produced zero emissions and the
  stored value froze at its last nonzero reading forever. Separately,
  `voice-room.ts`'s `onActiveSpeakers` removal branch reset `isSpeaking` but
  never `rmsLevel`, so a stale boosted value (`RMS_START_THRESHOLD + 0.05 =
0.11`) could also freeze above the 0.03 stop threshold after an
  active-speaker hangover flap. Fixed both: the analyser now emits exactly
  one trailing all-zero update when signal drops to silence
  (`analyserHadSignal` flag), and the active-speaker removal branch always
  zeros `rmsLevel`, not just when `isSpeaking` was true.
- `screen-share.spec.mjs` Direction B's `stop-share` was a **spec bug**: the
  peer sent `{ type: 'stop_share' }`, but `StopShare` carries an explicit
  `#[serde(rename = "stop-share")]` override (hyphen), asymmetric with
  `StartShare`'s plain `rename_all = "snake_case"` default
  (`shared/src/signaling/mod.rs`). The wrong tag parsed as an unknown
  message type and was silently dropped server-side, so the "waiting for
  stream..." indicator never cleared. Fixed by sending the real tag.
- `camera.spec.mjs` was also a **spec bug**, not a product bug: the capture/
  publish pipeline and the self-row "your camera is on" indicator both work
  correctly. The failure was asserting "No video active" reappears right
  after camera-off — at the e2e window's tier, `ActiveRoom.tsx`'s
  `groupedPanelVideoActivityKey` effect auto-switches the grouped panel away
  from VIDEO the instant `videoTilesById` empties (by design), so that
  placeholder is unreachable there without first re-selecting the VIDEO tab.
  Fixed by asserting the designed auto-switch (`aria-selected="false"` on
  the VIDEO tab) and then re-clicking it before checking the placeholder.

**A fourth finding, surfaced during the WebdriverIO migration (#297) — also
resolved:** the embedded WebDriver provider cannot deserialize DOM element
references returned directly from an injected `execute()` script — every
element in the return value comes back `null`
(confirmed directly: even `execute(() => document.body)` returns `null`).
`getByText(regexPattern)`'s server-side matching (`page-adapter.mjs`'s
`#rawAll()`, added to avoid an O(n) per-candidate `getText()` round-trip on
a busy room page) originally tried to return matched elements this way,
so **any regex-based locator's `waitFor()`/count assertions never actually
resolved a match, ever** — not intermittent, not disk- or load-related, a
deterministic 0-results bug hiding behind a 15s timeout that looked exactly
like flakiness. Root-caused via direct experimentation (`execute(() =>
[document.body]).length` returns `1`, but the element inside is `null`;
`execute(() => document.body)` alone is also `null`). Fixed by having the
script tag matched elements with a temporary attribute instead of returning
them, then fetching real, usable element handles for that attribute via a
normal `browser.$$()` selector query — the same mechanism every non-regex
locator path already uses successfully. If you're touching the regex-text
path in `page-adapter.mjs`, don't revert to returning elements directly from
`execute()` on this provider.

All specs, plus the full pre-existing suite, pass now.

**Rate limiting.** Two in-process, per-IP backend limiters bite during
suite runs, both on 1-hour sliding windows:

- **Registration** — 5/hour by default (`AuthRateLimiterConfig` in
  `wavis-backend/src/auth/auth_rate_limiter.rs`); `/auth/register` calls
  from both `registerAndLoginViaUi` and `seedChannelWithInvite` draw from
  the same window, at ~2 registrations per spec. Override:
  `AUTH_REGISTER_RATE_LIMIT`.
- **Recovery** — 30/hour by default (`RecoveryRateLimiterConfig` in
  `wavis-backend/src/auth/recovery_rate_limiter.rs`). Every Login-UI
  authentication (`loginViaUi`, both new-device and trusted paths) is a
  `/auth/recover` call, so repeated suite runs within an hour exhaust this
  one too — observed as `loginViaUi` timing out on `/login` while the
  backend logs `recover rate-limited (IP)` with a ~25-minute retry_after.
  Override: `AUTH_RECOVER_RATE_LIMIT`.

docker-compose.yml passes both through (defaults stay 5/30). For e2e use,
raise them when starting the stack:

```powershell
$env:AUTH_REGISTER_RATE_LIMIT="200"; $env:AUTH_RECOVER_RATE_LIMIT="200"; docker compose up -d --build
```

(`docker compose restart wavis-backend` alone only clears the in-memory
windows — enough when iterating on a single spec, not for a full-suite run.)

**Beyond auth: the join limiter, temp-ban list, and WS/chat/global limiters
have no env override at all or aren't wired into docker-compose.yml**
(`JoinRateLimiter`'s per-room/per-code ceilings, `TempBanList`'s 10-minute
IP ban after 5 violations, `WsRateLimiter`, `ChatRateLimiter`, both
`GlobalRateLimiter`s — see security.md's "Test-Only Rate Limit Bypass"
section). A full suite run can trip these — most notably `TempBanList`,
which silently bans the test runner's own IP for 10 minutes and breaks
every subsequent spec with generic timeouts, not an auth error. For a
full-suite run, build the backend with the `test-no-rate-limits` cargo
feature instead of trying to raise every individual limiter (this repo's
`tools/dev/start-backend-e2e.ps1` wraps exactly this recipe, plus the two
env vars above, plus a `-Restore` flag to rebuild without the feature
afterward):

```powershell
$env:CARGO_FEATURES="test-no-rate-limits"; docker compose up -d --build wavis-backend
```

Rebuild **without** that env var afterward to restore normal enforcement —
this feature is compile-time gated (never touches a production build), but
the container it produces has no rate limiting at all, so don't leave it
running as your regular dev backend.

**Multi-tier DOM duplication.** `ActiveRoom` mounts `ChatPanel`/
`LogsPanel`/`ParticipantsPanel` once per responsive layout tier (mobile /
intermediate / desktop) and hides the inactive tiers with CSS rather than
unmounting them. Plain `getByText`/`getByPlaceholder`/`getByRole` locators
can silently resolve to a CSS-hidden duplicate instead of the one actually
on screen — `.first()` alone only picks DOM order, not visibility, and DOM
order does not reliably match which tier is rendered. `visibleText()` and
the `:visible`-filtered locators in `live-backend-helpers.mjs` exist
specifically to dodge this; reuse them for anything inside `/room` rather
than reaching for a raw `getByText`.

## Media quality + network simulation

`zz-network-quality.spec.mjs` (named `zz-` so it runs last in the suite's
alphabetical single-worker order — a netem teardown failure then can't
contaminate any other spec) proves media stays smooth under a good network,
degrades observably but keeps flowing under a simulated bad one, and
recovers once the bad network clears. `audio-received.spec.mjs` and
`screen-share-audio-not-heard-until-opened.spec.mjs` already prove media
_arrives_; this is the smoothness/resilience proof neither one covers.

**Why netem on the LiveKit container, not client-side throttling.**
WebdriverIO/WebDriver has no network-condition emulation API that reaches
WebRTC's UDP media path (Chrome DevTools Protocol's own
`Network.emulateNetworkConditions`, which the old CDP-based harness could
have reached for, only ever intercepted HTTP(S)/fetch/XHR traffic — never
touched UDP either). The only leg that's actually shapeable is the LiveKit
container's own egress: `docker-compose.yml`'s `livekit` service gets
`cap_add: [NET_ADMIN]` and installs `iproute2` (`tc`) once at container
start (before the existing config-templating `sed` + `exec
/livekit-server`). Since LiveKit → GUI downstream is exactly the "incoming
media" leg these specs want to degrade, shaping the container's `eth0` with
`tc qdisc ... netem` is sufficient — no client-side or host-level shaping
needed.

**Helpers** (`live-backend-helpers.mjs`):

- `hasNetemSupport()` — `docker exec wavis-livekit sh -c "command -v tc"`;
  specs skip-with-reason (not fail) if this is false, e.g. the container
  started offline and `apk add` never ran.
- `setNetworkConditions({ delayMs, jitterMs, lossPct, rateKbit })` — `docker
exec wavis-livekit tc qdisc replace dev eth0 root netem ...`.
- `clearNetworkConditions()` — `tc qdisc del dev eth0 root`, tolerating a
  "no qdisc" error so it's safe to call unconditionally (e.g. in a `finally`,
  even if a prior run crashed mid-shape).

**Video peer.** `livekit-tone-peer.mjs`'s `connectTonePeer()` gained
`startVideoPattern()`/`stopVideoPattern()`, publishing a real 640×360 @15fps
moving RGBA pattern under `TrackSource.SOURCE_SCREENSHARE` (real frames that
actually change, so the encoder can't collapse them to near-zero bitrate).
No Watch All / share-tile step is needed first — unlike screen-share
audio-only (deliberately gated behind explicit viewer intent, see issue
#174's spec), `ScreenShare` video publications are subscribed unconditionally
in `TrackPublished` (`livekit-media.ts` ~2740-2765), and the stats-interval's
video receiver polling only needs `publication.track` to be present — which
subscription alone provides.

**Stats plumbing.** `App.tsx`'s `VITE_DIAGNOSTICS`-gated diagnostics tick
already read both `networkStats` (voice-room.ts) and `videoReceiveStats`
(livekit-media.ts) every second, but only forwarded them to the Tauri
`diagnostics:voice-stats` event — `window.__wavisVoiceStats` (the hook e2e
specs actually read) carried only `participants`/`selfParticipantId`. Both
fields were widened onto `window.__wavisVoiceStats` too — additive,
production-absent (same `VITE_DIAGNOSTICS` gate as before), and **requires
rebuilding the debug exe** (`npm run build:app` — see "Build the app to
test against" above) before the suite sees them; a stale debug exe will
read `undefined` for both new fields.

**Thresholds.** The "good" vs "degraded" line reuses
`DiagnosticsPage.tsx`'s own bad-network thresholds (concealment >10
events/interval, packet loss >5%), and the degraded-phase assertion compares
against the spec's own recorded baseline (`>`, not a fixed magic number) to
avoid flake from any single noisy 10s stats interval.

**Verify manually if this ever needs re-diagnosing:** with a real call up,
`docker exec wavis-livekit tc qdisc replace dev eth0 root netem loss 30%`
should visibly degrade audio within a few seconds, and `docker exec
wavis-livekit tc qdisc del dev eth0 root` should restore it.

## Deferred: CI

No GitHub Actions job runs any of this yet, and none of `.github/workflows/`
uses a `windows-latest`/Linux-with-GUI runner today (desktop builds go
through Codemagic, not GHA) — this is issue #306's job once the local suite
is proven stable on every platform it targets (this repo's own established
local-first/CI-later precedent for this harness), not this ticket's. The
target design, once ready:

- A CI job (Windows and/or Linux) mirroring `workspace-ci.yml`'s `test-db`
  job shape — ephemeral Postgres + LiveKit + `wavis-backend` as CI
  `services:`, torn down automatically.
- `npm run build:app` in-job, then seed the settings store with a valid
  session before `launchApp()` (again: runtime setting, not an env var —
  this is real setup work, not a config flag).
- Run the full suite, including live-backend specs, against that ephemeral
  backend.
- A **fake/mock backend** (canned WebSocket signaling, demo participants/
  chat data) was considered and explicitly rejected for this — it would
  reopen a real product-surface decision (a permanent Demo Mode) that a
  prior session deliberately scoped out, as a side effect of test tooling
  rather than its own call.

Open unknowns that make this a spike, not a copy-paste of `test-db`: webview
CDP/WebDriver behavior on a GH-hosted runner is unverified for either
platform, and so is real WebRTC media in that sandbox specifically —
`audio-received.spec.mjs` confirms real media works locally (this machine,
with the driver's `--disable-features=WebRtcHideLocalIpsWithMdns` flag and
`deploy/livekit.yaml`'s `use_external_ip: false`/`node_ip` fix — see "Known
local media blocker" above), but a GH-hosted runner's network/sandbox
constraints (UDP egress, NAT type, whether the same mDNS-disable flag is
even necessary there, WSLg-equivalent audio/GPU access on a Linux runner)
are a separate unverified question. Don't wire this up without confirming
both first.

## Other possible follow-ups (not built here)

- Give `DeviceSetup` an invite-code field so UI-driven registration
  (`registerViaUi`) works against a backend that requires one, and
  `login.spec.mjs` can cover the registration flow again — see the "Known
  gap" note above for the current REST-based workaround.
- macOS support (#298) — WKWebView driver wiring, building on this
  migration's shared framework/adapter work.
