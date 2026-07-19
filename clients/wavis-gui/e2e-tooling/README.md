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
`screen-share.spec.mjs`, `camera.spec.mjs`, `two-instances.spec.mjs` — see
"Live-backend specs" below for the extra build step and backend they
require. These also `test.skip()` when the main window is mid-call, for the
same reason. `two-instances.spec.mjs` additionally uses the `appB` fixture
(a second real `launchApp()`) — see "Two simultaneous GUI instances" below.

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
  entry — is handled automatically: `launchApp()` defaults
  `keyringService` to `com.wavis.gui.e2e` for every launch. This build-time
  var is the baseline for a single instance; two-instance specs additionally
  override it per-launch at runtime — see "Two simultaneous GUI instances"
  below.)

```powershell
$env:VITE_ALLOW_INSECURE_TLS="true"; $env:VITE_AUTH_STORE_NAME="wavis-auth-e2e.json"; npx tauri build --debug
```

This **replaces** `target/debug/wavis-gui.exe` — the 4 backend-independent
specs still run fine against it, but going back to plain manual
verification (a build that reuses your real persisted login) needs a
rebuild without these two env vars.

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

- `WEBVIEW2_USER_DATA_FOLDER` — always a fresh temp dir per launch, so two
  processes never share a WebView2 profile.
- `port` — give the second instance its own CDP port (`9223`; `9222` stays
  the default for `port`-less calls).
- `authStoreName` — **new.** Without this, both instances share one Tauri
  store file (`wavis-auth.json`/`wavis-auth-e2e.json`, whichever the exe was
  built with) and race last-writer-wins on login state. Passing a distinct
  name makes `launchApp()` set `WAVIS_AUTH_STORE_NAME` per-process, which
  `auth.ts`'s `resolveStoreName()` reads at runtime via a Tauri command
  (`get_auth_store_name` in `src-tauri/src/main.rs`) — it wins over the
  build-time `VITE_AUTH_STORE_NAME` baked into the exe.
- `keyringService` — **new.** Same problem, one level down: without this,
  both instances share one OS keychain refresh-token entry and a token
  rotation in one can invalidate the other mid-test. `keyring_service()` in
  `main.rs` already read `WAVIS_KEYRING_SERVICE` at runtime; `launchApp()`
  now exposes it as a per-call option instead of hardcoding one value for
  every launch.

`wavis-settings.json` (audio/video prefs) stays **shared on purpose** — it's
prefs-only, not identity, and isolating it wasn't needed for anything this
spec asserts.

**The account rule.** Two different accounts in the same voice channel never
displace each other. The SAME account joining the same voice channel twice
(regardless of device/window) always does — that's the server's intended
"ghost duplicate" prevention, not something to route around. A two-instance
spec that wants no displacement needs two REAL DIFFERENT accounts, one per
instance.

Ad-hoc snippet (outside the formal suite, e.g. a scratchpad script):

```js
import { launchApp } from './driver.mjs';

const a = await launchApp();
const b = await launchApp({
  port: 9223,
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
section). A full 20-test suite run can trip these — most notably
`TempBanList`, which silently bans the test runner's own IP for 10 minutes
and breaks every subsequent spec with generic timeouts, not an auth error.
For a full-suite run, build the backend with the `test-no-rate-limits`
cargo feature instead of trying to raise every individual limiter:

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

**Why netem on the LiveKit container, not CDP/Playwright throttling.**
Playwright's own network-condition APIs (and CDP's `Network.emulateNetworkConditions`
generally) only intercept the page's HTTP(S)/fetch/XHR traffic — they do not
touch WebRTC's UDP media path at all, confirmed against the driver's CDP
session. The only leg that's actually shapeable is the LiveKit container's
own egress: `docker-compose.yml`'s `livekit` service gets `cap_add:
[NET_ADMIN]` and installs `iproute2` (`tc`) once at container start (before
the existing config-templating `sed` + `exec /livekit-server`). Since
LiveKit → GUI downstream is exactly the "incoming media" leg these specs
want to degrade, shaping the container's `eth0` with `tc qdisc ... netem` is
sufficient — no client-side or host-level shaping needed.

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
specs actually read, since `window.__TAURI__` isn't exposed) carried only
`participants`/`selfParticipantId`. Both fields were widened onto
`window.__wavisVoiceStats` too — additive, production-absent (same
`VITE_DIAGNOSTICS` gate as before), and **requires rebuilding the debug exe**
(`npx tauri build --debug` with the usual e2e env vars — see "Building the
debug exe" above) before the suite sees them; a stale debug exe will read
`undefined` for both new fields.

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

- Give `DeviceSetup` an invite-code field so UI-driven registration
  (`registerViaUi`) works against a backend that requires one, and
  `login.spec.mjs` can cover the registration flow again — see the "Known
  gap" note above for the current REST-based workaround.
