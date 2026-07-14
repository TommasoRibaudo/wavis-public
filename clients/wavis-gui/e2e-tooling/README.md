# Wavis GUI e2e-tooling

Dev-only harness for launching and driving the real Wavis desktop app
(Tauri 2.0) — not a test suite, not shipped with the app. Use it to click
through and verify a GUI change actually works instead of relying only on
`tsc`/`eslint`/`vitest`/production build.

This is deliberately **not** wired into `clients/wavis-gui/package.json` or
CI — it's a standalone tool with its own `package.json` so it never affects
the app's own dependency tree or lockfile.

## How this works

**Primary path — real Playwright over CDP.** WebView2 (the Chromium-based
engine Tauri uses on Windows) exposes a native Chrome DevTools Protocol
debugging port via the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<port>`
environment variable — a standard Microsoft WebView2 dev/diagnostics flag,
not Tauri-specific. Playwright is CDP-native (`chromium.connectOverCDP()`),
so it attaches directly to that port with **no WebDriver-protocol
translation layer involved** — that translation layer is exactly where an
earlier `tauri-driver` attempt hit a dead end (see
`webdriver-spike-DEAD-END.mjs` for the full repro: it connected but the
browsing context stayed permanently `about:blank`).

This actually works, confirmed by direct testing: real DOM access, real
`.click()`/`.fill()`/`.locator()`, real screenshots, and **real multi-window
support** — each Tauri window (`main`/`/room`, `diagnostics`, `watch-all`,
etc.) shows up as its own Playwright `Page` in `context.pages()`, no plugin
or Tauri-side change required. Confirmed live-state tracking too: the pages
reflect the app's actual real-time connection state (e.g. "connecting" →
"offline"/"joined"), not a stale snapshot.

**Zero product-code changes** for the base connection — the env var is set
when spawning the process, nothing in `src-tauri/` needs to change (and
nothing there currently overrides `additional_browser_args`, confirmed via
grep, so nothing conflicts with it).

**Fallback path — Win32 screenshot-only** (`launchAppWin32` +
`listWindows`/`screenshotWindow` in `driver.mjs`). Launches the exe as a
plain child process, enumerates its real windows via Win32 `EnumWindows`,
screenshots via `PrintWindow`. No DOM interaction, but useful if CDP can't
reach a specific window for some reason. This was the _only_ working path
before the CDP approach was found — kept for that reason.

## One-time setup (per machine)

`npm install` from this directory (`playwright-core` for the CDP path,
`webdriverio` kept only for `webdriver-spike-DEAD-END.mjs` reference). No
`tauri-driver`/`msedgedriver` install needed — that was only for the dead
WebDriver path.

## Build the app to test against

```
cd clients/wavis-gui
npx tauri build --debug
```

Produces `target/debug/wavis-gui.exe` **at the Cargo workspace root**
(`clients/wavis-gui/src-tauri` is a workspace member, so output lands in the
workspace's shared `target/`, not under `src-tauri/target/`). `driver.mjs`
always launches whatever's currently on disk at that path. **Re-run this
after any source change you want to verify.**

(Use `--debug` for iteration — much faster than a full release build.)

You'll see the command end with an error about `TAURI_SIGNING_PRIVATE_KEY`
("A public key has been found, but no private key") — that's the
updater-bundle (msi/nsis) signing step failing, which is expected and
harmless here: it happens _after_ the exe itself is already built
("Finished `dev` profile... Built application at: .../wavis-gui.exe"), and
this harness only needs the raw exe, not a signed installer bundle.

**Note:** this debug build shares the same OS app-data identifier
(`com.wavis.desktop`) as any normal Wavis install on this machine, so it
reuses real persisted login/session state — you'll see whatever
account/rooms are already logged in, not a blank first-run state. It also
talks to a **real backend** — UI state reflects real connection status
(connecting/offline/joined), not canned data.

## Usage

```js
import { launchApp } from './driver.mjs';

const app = await launchApp(); // spawns with CDP enabled, connects Playwright, ~4s settle
try {
  console.log(app.pages().map((p) => p.url())); // e.g. ['http://tauri.localhost/room', '.../diagnostics']

  const main = app.page(); // main window, or app.getPageByPath('/watch-all') etc.
  await main.screenshot({ path: 'C:/path/to/scratchpad/out.png' });

  // Real interaction — standard Playwright API from here:
  await main.getByText('/settings', { exact: true }).first().click();
  const text = await main.locator('body').innerText();
} finally {
  await app.close(); // always — otherwise the app process + CDP connection linger
}
```

Then **Read the PNG and actually look at it** — a blank/error frame is a
failure to launch, not a pass. Write this as a throwaway script in
scratchpad, not a committed file.

**Locator tip learned the hard way:** Wavis's terminal-style UI repeats
short command labels (`/settings`, `/mute`, etc.) in multiple places — use
`.first()`/`.nth()` or a more specific locator when Playwright's strict
mode reports multiple matches, same as you would on any real app.

## Multi-window

Confirmed working directly: `app.pages()` / `context.pages()` shows every
open Tauri window as its own Playwright page, keyed by URL path (`/room`,
`/diagnostics`, `/watch-all`, `/screen-share-*`, `/camera-popout`,
`/share-picker`, `/share-indicator` — see `src-tauri/capabilities/default.json`
for the full window-label list; the CDP-visible URL path is what
`getPageByPath()` matches against). New windows that open mid-session
(e.g. clicking something that opens Watch All) will appear in
`context.pages()` — listen for `context.on('page', ...)` if you need to
catch one as it opens rather than polling.

The `diagnostics` window is a free, always-available second window for
exercising the multi-window path without scripting any prior click — it
auto-opens on launch because this debug build bakes in `VITE_DIAGNOSTICS=true`.

## Formal test suite (`tests/`)

`npm run test` (from this directory) runs `tests/*.spec.mjs` via
`@playwright/test` (`playwright.config.mjs`: `testDir: './tests'`,
`workers: 1` — the app launch is stateful and pinned to a single CDP port,
so parallel workers would collide). Every spec imports `test`/`expect` from
`tests/fixtures.mjs`, not `@playwright/test` directly — the `app` fixture
there wraps `launchApp()`/`app.close()` so lifecycle is never duplicated
per-file. No `npx playwright install` browser download is needed: specs
never touch Playwright's own `browser`/`context`/`page` fixtures, only the
CDP connection `driver.mjs` already makes.

**Prerequisite:** build the debug exe first (`npx tauri build --debug` from
`clients/wavis-gui`, see above) — `npm run test` launches whatever's
currently on disk.

Backend-independent coverage: `launch.spec.mjs`, `title-bar.spec.mjs`,
`multi-window.spec.mjs`, `settings.spec.mjs`. None of these assume a
particular login/session state — `settings.spec.mjs` explicitly
`test.skip()`s with a stated reason if the persisted session on the machine
doesn't put the AuthGate `/settings` link in reach (unauth'd routes, or main
window already in an active room), rather than assuming one state and
flaking on the other.

Live-backend coverage: `login.spec.mjs`, `room-join.spec.mjs`,
`chat.spec.mjs`, `participants.spec.mjs`, `audio-received.spec.mjs`,
`screen-share.spec.mjs`, `camera.spec.mjs` — see "Live-backend specs" below
for the extra build step and backend they require. These also
`test.skip()` when the main window is mid-call, for the same reason.

`clients/wavis-gui/vite.config.ts`'s vitest `test.exclude` explicitly
excludes `e2e-tooling/**` — without it, vitest's default `*.spec.*` glob
would try to collect these Playwright specs when `npm test` runs from
`clients/wavis-gui`.

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

**2. A debug exe built for live-backend use.** Two build-time env vars, on
top of the usual `npx tauri build --debug`:

- `VITE_ALLOW_INSECURE_TLS=true` — the local backend serves plain HTTP
  (`http://localhost:3000`), and `DeviceSetup`/`Login` reject `http://`
  server URLs unless this is set (default `false`).
- `VITE_AUTH_STORE_NAME=wavis-auth-e2e.json` — points the Tauri auth store
  at a file distinct from `wavis-auth.json`, so registering/logging in
  during these specs never overwrites a real persisted session on this
  machine. (The other half of that isolation — the OS keychain refresh-token
  entry — is handled automatically: `driver.mjs` sets
  `WAVIS_KEYRING_SERVICE=com.wavis.gui.e2e` for every launch, unconditionally.)

```powershell
$env:VITE_ALLOW_INSECURE_TLS="true"; $env:VITE_AUTH_STORE_NAME="wavis-auth-e2e.json"; npx tauri build --debug
```

This **replaces** `target/debug/wavis-gui.exe` — the 4 backend-independent
specs still run fine against it, but going back to plain manual
verification (a build that reuses your real persisted login) needs a
rebuild without these two env vars.

Each live-backend spec that needs a GUI-side authenticated identity gets one
by registering a fresh, timestamp-suffixed throwaway account via the real
`DeviceSetup` UI (`registerViaUi` in `tests/live-backend-helpers.mjs`) —
registration has no email/CAPTCHA step, so this is cheap. `login.spec.mjs`
is the one spec that exercises that UI as the thing under test; the other
three treat it as setup. Channel/invite seeding for `room-join`/`chat`/
`participants` uses REST (`POST /auth/register_device`,
`POST /channels`, `POST /channels/:id/invites` — see `doc/QUICKSTART.md`
§11) for the channel _owner_, so that identity never needs to touch the GUI
at all.

**Second participant (chat/participants specs).** These need a second
connected participant to assert anything meaningful. Two real
Playwright-driven GUI instances was considered and is unbuilt/unproven — no
prior art for two concurrent `launchApp()` calls (port/profile conflicts
unexplored). Instead, `spawnPeer()` drives `ws-sfu-test`
(`scripts/ws-sfu-test`) in its interactive JSON-line mode as a scriptable
second signaling participant. This was chosen over `wavis-client`'s REPL
because `wavis-client` has no chat send/receive support at all (checked its
command parser — `create`/`join`/`invite`/`revoke`/`leave`/`status`/`name`/
`volume`/`help`/`quit` only). These specs verify the GUI's own rendering
reacts correctly to a second real participant — they do not verify a second
real GUI instance's own rendering.

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
  `evaluate()` calls. Needed because `window.__TAURI__` isn't exposed
  (no `withGlobalTauri`), so Playwright can't read the
  `diagnostics:voice-stats` Tauri event directly. **Production-absent**:
  only assigned inside the existing diagnostics interval, gone in real
  release builds.
- `driver.mjs`'s `--use-fake-ui-for-media-stream` launch flag. Joining voice
  publishes the GUI's own mic (and `camera.spec.mjs` toggles its own
  camera), which triggers WebView2/Chromium's own in-browser `getUserMedia`
  permission prompt — a different mechanism from the Windows-level camera/
  mic privacy toggle (already "Allow" by default; not the source). Without
  this flag every media spec blocks on a manual click, and granting
  mid-assertion was observed to perturb the GUI's audio pipeline. This is
  the standard Chromium testing flag for auto-accepting capture permission
  prompts with no UI — capture still uses real devices, only the permission
  UI is suppressed, and it's scoped to e2e launches only (same as the mDNS
  flag below).

`audio-received.spec.mjs` is the load-bearing proof that real WebRTC media
works in the CDP-launched debug exe (see "Known local media blocker" below
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
- Real watched pixels for screen share (a peer that actually publishes
  video, and the GUI's Watch All viewer path) and a remote camera tile —
  both need a video-publishing peer via `@livekit/rtc-node`'s
  `VideoSource`. This was scoped as a timeboxed stretch and not attempted
  in the session that added `audio-received.spec.mjs`; the concrete blocker
  to resolve first is matching the track source/name conventions the GUI's
  `onScreenShareSubscribed` (`voice-room.ts`) keys on against what
  `clients/shared/src/livekit_connection.rs`'s Rust publisher actually sends.
- `screen-share.spec.mjs`'s Direction B (peer signals `start_share` with no
  media) intentionally only asserts the "waiting for stream..." indicator —
  there is no track to watch, by construction.

**Known local media blocker — RESOLVED.** Real WebRTC media over the
default local `docker compose up` stack initially failed for every spec
that needs it. Real diagnosis, not speculation (a throwaway
`main.on('console', ...)` + LiveKit-server-log capture was used to trace
it), needed two fixes together:

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
passes in full. Verify with `npx playwright test
tests/audio-received.spec.mjs` — must connect and read a real decoded
`rmsLevel` within 20s.

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

All three specs, plus the full pre-existing suite, pass now.

**Rate limiting.** The backend's register/register_device rate limiter is
in-process, per-IP, and allows only **5 registrations per hour** by default
(`AuthRateLimiterConfig` in `wavis-backend/src/auth/auth_rate_limiter.rs`) —
and `/auth/register` (each spec's `registerViaUi`) and
`/auth/register_device` (each spec's `seedChannelWithInvite` owner) draw
from the **same** window. At ~2 registrations per spec, one full 7-spec run
needs ~14, so specs past the first two or three fail setup with `429`
(`register_device failed: 429` from `live-backend-helpers.mjs`) even on a
freshly restarted backend. For e2e use, raise the limit when starting the
stack — the backend reads `AUTH_REGISTER_RATE_LIMIT` and docker-compose.yml
passes it through (default stays 5):

```powershell
$env:AUTH_REGISTER_RATE_LIMIT="200"; docker compose up -d --build
```

(`docker compose restart wavis-backend` alone only clears the in-memory
window — enough when iterating on a single spec, not for a full-suite run.)

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

## Deferred: CI

No GitHub Actions job runs any of this yet, and none of `.github/workflows/`
uses a `windows-latest` runner today (desktop builds go through Codemagic,
not GHA) — this would be new territory on multiple axes at once. The
target design, once the local suite has proven stable:

- A `windows-latest` job mirroring `workspace-ci.yml`'s `test-db` job shape
  — ephemeral Postgres + LiveKit + `wavis-backend` as CI `services:`,
  torn down automatically.
- `npx tauri build --debug` in-job, then seed the settings store with a
  valid session before `launchApp()` (again: runtime setting, not an env
  var — this is real setup work, not a config flag).
- Run the full suite, including live-backend specs, against that ephemeral
  backend.
- A **fake/mock backend** (canned WebSocket signaling, demo participants/
  chat data) was considered and explicitly rejected for this — it would
  reopen a real product-surface decision (a permanent Demo Mode) that a
  prior session deliberately scoped out, as a side effect of test tooling
  rather than its own call.

Open unknowns that make this a spike, not a copy-paste of `test-db`:
WebView2 CDP behavior on a GH-hosted Windows runner is unverified, and so is
real WebRTC media in that sandbox specifically — `audio-received.spec.mjs`
confirms real media works locally (this machine, with the driver's
`--disable-features=WebRtcHideLocalIpsWithMdns` flag and
`deploy/livekit.yaml`'s `use_external_ip: false`/`node_ip` fix — see "Known
local media blocker" above), but a GH-hosted runner's network/sandbox
constraints (UDP egress, NAT type, whether the same mDNS-disable flag is
even necessary there) are a separate unverified question. Don't wire this up
without confirming both first.

## Other possible follow-ups (not built here)

- A `WEBVIEW2_USER_DATA_FOLDER`/port-conflict story for running multiple
  harness instances truly in parallel (currently: unique temp profile dir
  per launch already avoids profile locking; port is fixed at 9222 by
  default but overridable via `launchApp({ port })`).
