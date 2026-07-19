// Shared helpers for the live-backend specs (login/room-join/chat/participants
// .spec.mjs). These need a reachable backend — see e2e-tooling/README.md's
// "Live-backend specs" section for the local docker-compose runbook.
//
// Not used by the backend-independent specs (launch/title-bar/multi-window/
// settings) — those stay dependency-free on purpose.
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const execFileAsync = promisify(execFile);

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// e2e-tooling/tests -> e2e-tooling -> wavis-gui -> clients -> workspace root
const WORKSPACE_ROOT = path.resolve(__dirname, '..', '..', '..', '..');

export const SERVER_URL = (process.env.E2E_SERVER_URL || 'http://localhost:3000').replace(
  /\/+$/,
  '',
);
const WS_URL = SERVER_URL.replace(/^http/, 'ws') + '/ws';

/**
 * ActiveRoom mounts ChatPanel/LogsPanel/ParticipantsPanel once per
 * responsive layout tier (mobile/intermediate/desktop — see
 * ActiveRoom.tsx) and hides the inactive ones with CSS instead of
 * unmounting them. `.first()` alone is NOT a safe way to pick the real,
 * on-screen instance — it picks DOM order, which may just as easily be a
 * CSS-hidden duplicate (confirmed: a `.first()` click resolved to a hidden
 * element here during testing). Intersecting with Playwright's `:visible`
 * locator, THEN taking `.first()`, is what actually targets the one on
 * screen.
 */
export function visibleText(page, textOrPattern) {
  return page.getByText(textOrPattern).and(page.locator(':visible'));
}

/** Same tier-duplication problem as visibleText, but for elements matched by their `title` attribute. */
export function visibleTitle(page, title) {
  return page.getByTitle(title).and(page.locator(':visible'));
}

/**
 * For SVG icon indicators (ParticipantRow's lucide camera/screen-share
 * icons): their accessible name comes from an aria-label or a nested SVG
 * <title> CHILD ELEMENT, not a `title` attribute, so getByTitle/visibleTitle
 * can never match them (commit 23719f1's glyph→lucide swap converted the
 * old title attributes). Playwright's role engine computes the name from
 * both sources, so match by role=img instead. Extra trap getByTitle also
 * had: it substring-matches, so 'you are sharing' would false-positive on
 * the adjacent title="you are sharing audio" badge — role-name matching is
 * whole-string and avoids that.
 */
export function visibleIcon(page, name) {
  return page.getByRole('img', { name }).and(page.locator(':visible'));
}

/**
 * A participant's row in ParticipantsPanel (ParticipantRow.tsx — a
 * `role="button"` div wrapping a `[+]`/`[-]` expand indicator and the
 * display name). Prefer this over `visibleText(page, displayName)` for
 * "is this participant showing in the room" assertions: a plain text match
 * also matches the transient "{name} joined" toast notification, which can
 * still be on screen right after a join and makes the locator resolve to 2
 * elements (Playwright strict-mode violation) instead of the persistent row.
 */
export function participantRow(page, displayName) {
  const escaped = displayName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return page.getByRole('button', { name: new RegExp(escaped) }).and(page.locator(':visible'));
}

/* ─── REST setup (backend-side identities/channels — no GUI involved) ──── */

async function postJson(pathname, body, accessToken) {
  const res = await fetch(`${SERVER_URL}${pathname}`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    throw new Error(`POST ${pathname} failed: ${res.status} ${await res.text()}`);
  }
  return res.json();
}

/** Poll GET /health until it responds ok, or throw after timeoutMs. */
export async function waitForBackendHealth(timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastErr;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${SERVER_URL}/health`);
      if (res.ok) return;
    } catch (err) {
      lastErr = err;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(
    `Backend at ${SERVER_URL} did not become healthy within ${timeoutMs}ms` +
      (lastErr ? ` (last error: ${lastErr.message})` : ''),
  );
}

/**
 * POST /auth/register — mints a full closed-alpha session with no UI step.
 * Use this for identities that never need to touch the GUI (e.g. a channel
 * owner in room-join.spec.mjs), not as a substitute for exercising the real
 * auth UI (that's what registerViaUi / login.spec.mjs are for).
 *
 * Also used as the D2 workaround (see README.md's "Known gaps" section):
 * DeviceSetup's registration UI has no invite-code field, so registerViaUi
 * 401s against a backend that requires one (routes.rs's `register` handler).
 * registerAndLoginViaUi below composes this REST call with loginViaUi to get
 * a real GUI-authenticated session anyway. Returns `phrase`/`username` too
 * (additive) so callers can log the same identity in a second time.
 */
export async function registerDevice() {
  const inviteCode = process.env.ALPHA_INVITE_CODE;
  if (!inviteCode) throw new Error('ALPHA_INVITE_CODE must be set for REST auth seeding');
  const suffix = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  const phrase = `e2e-password-${suffix}`;
  const username = `E2E-${suffix}`;
  const res = await fetch(`${SERVER_URL}/auth/register`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ phrase, username, inviteCode }),
  });
  if (!res.ok) throw new Error(`register failed: ${res.status}`);
  const body = await res.json(); // { user_id, device_id, recovery_id, access_token, refresh_token }
  return { ...body, phrase, username };
}

export async function createChannel(accessToken, name) {
  return postJson('/channels', { name }, accessToken); // { channel_id, ... }
}

export async function createInvite(
  accessToken,
  channelId,
  { expiresInSecs = 3600, maxUses = 5 } = {},
) {
  return postJson(
    `/channels/${channelId}/invites`,
    { expires_in_secs: expiresInSecs, max_uses: maxUses },
    accessToken,
  ); // { code, channel_id, ... }
}

/** REST-seeds a channel + invite owned by a fresh closed-alpha identity. */
export async function seedChannelWithInvite(name) {
  const owner = await registerDevice();
  const channel = await createChannel(owner.access_token, name);
  const invite = await createInvite(owner.access_token, channel.channel_id);
  return { owner, channel, invite };
}

/* ─── UI-driven registration (the GUI-side authenticated identity) ─────── */

/**
 * Drives the real DeviceSetup registration flow (step 1 -> 2 -> 3 ->
 * continue) and returns the scraped recovery ID. Requires the debug exe to
 * have been built with VITE_ALLOW_INSECURE_TLS=true (see README) so the
 * insecure-TLS toggle is present for a plain-http serverUrl.
 *
 * `inviteCode` is required: DeviceSetup's registration form validates it
 * client-side before submitting (see `validateInviteCode` in
 * `DeviceSetup.tsx`), and `POST /auth/register` 401s without one against a
 * closed-alpha backend. No spec currently uses this — specs that need a
 * GUI-authenticated identity use `registerAndLoginViaUi` (REST registration
 * + real Login UI) instead, since it doesn't need a live invite code passed
 * through Playwright. Use this one when the thing under test is the
 * registration UI itself.
 */
export async function registerViaUi(
  page,
  { username, password, inviteCode, serverUrl = SERVER_URL } = {},
) {
  // A stale wavis-auth-e2e.json (stored recovery ID from a previous run, but
  // a session the backend no longer knows — e.g. the docker DB was recreated
  // since) lands the app on /login instead of /setup. Trusted-device mode
  // shows only a password field, so the Username fill below would hang
  // forever. Navigate out the same way login.spec.mjs's getToSetup does:
  // "Not you / log in on a new device" swaps in the /setup button (they're
  // the two branches of one ternary — Login.tsx). The try/catch covers
  // new-device mode, where "Not you" doesn't exist and /setup is direct.
  if (new URL(page.url()).pathname.startsWith('/login')) {
    try {
      await page
        .getByText('Not you / log in on a new device', { exact: true })
        .click({ timeout: 10_000 });
    } catch {
      // Already in new-device mode — /setup is directly visible.
    }
    await page.getByText('/setup', { exact: true }).click();
  }

  await page.getByLabel('Username', { exact: true }).fill(username);
  await page.getByLabel('Password', { exact: true }).fill(password);
  await page.getByLabel('Confirm Password', { exact: true }).fill(password);
  await page.getByText('/next', { exact: true }).click();

  await page.getByLabel('Server URL', { exact: true }).fill(serverUrl);
  await page.getByLabel('Invite Code', { exact: true }).fill(inviteCode);
  if (serverUrl.startsWith('http://')) {
    await page.getByText('--danger-insecure-tls', { exact: true }).click();
  }
  await page.getByText('/register', { exact: true }).click();

  const recoveryIdLocator = page.locator('code').first();
  await recoveryIdLocator.waitFor({ state: 'visible', timeout: 15_000 });
  const recoveryId = (await recoveryIdLocator.innerText()).trim();

  await page.getByText('I have saved my Recovery ID (required)', { exact: false }).click();
  await page.getByText('/continue', { exact: true }).click();

  return { recoveryId };
}

/**
 * Drives the real Login UI's "new device" path (Wavis ID + Password + Server
 * URL) to authenticate an identity that was created out-of-band (REST
 * registerDevice() — the D2 workaround, see registerDevice's doc comment).
 * Unlike registerViaUi, /login is a top-level route with no auth-gate
 * redirect (see routes.ts), so it's reachable via a direct navigation even
 * on a device that has never registered locally — no "Not you" detour
 * needed, since a fresh authStoreName store has no locally-saved recovery ID
 * and Login.tsx already defaults to new-device mode (deriveLoginMode).
 */
export async function loginViaUi(page, { recoveryId, password, serverUrl = SERVER_URL } = {}) {
  if (new URL(page.url()).pathname !== '/login') {
    await page.goto(new URL('/login', page.url()).href);
  }

  // Trusted-device mode (a locally-saved recovery ID from a prior run in
  // this same store) shows only a password field — same "Not you" detour
  // registerViaUi/getToSetup use to reach the full new-device form.
  if ((await page.getByLabel('Wavis ID', { exact: true }).count()) === 0) {
    try {
      await page
        .getByText('Not you / log in on a new device', { exact: true })
        .click({ timeout: 10_000 });
    } catch {
      // Already in new-device mode.
    }
  }

  await page.getByLabel('Wavis ID', { exact: true }).fill(recoveryId);
  await page.getByLabel('Password', { exact: true }).fill(password);
  await page.getByLabel('Server URL', { exact: true }).fill(serverUrl);
  if (serverUrl.startsWith('http://')) {
    // Unlike DeviceSetup's insecure-TLS checkbox (always starts unchecked —
    // useState(false), no store read), Login.tsx's version is initialized
    // from the persisted `insecure_tls` store value on mount
    // (getInsecureTls() in its load effect). Since this is a real checkbox
    // toggle, clicking unconditionally can turn an already-ON value OFF if a
    // prior run in this same auth store left it set — check state first.
    const insecureTlsCheckbox = page.locator(
      'label:has-text("--danger-insecure-tls") input[type="checkbox"]',
    );
    if (!(await insecureTlsCheckbox.isChecked())) {
      await page.getByText('--danger-insecure-tls', { exact: true }).click();
    }
  }
  await page.getByText('/login', { exact: true }).click();

  await page.waitForURL((url) => url.pathname !== '/login', { timeout: 15_000 });
}

/** Composes REST registerDevice() (D2 workaround) with loginViaUi for a real GUI-authenticated session. */
export async function registerAndLoginViaUi(page, { serverUrl = SERVER_URL } = {}) {
  const identity = await registerDevice();
  await loginViaUi(page, {
    recoveryId: identity.recovery_id,
    password: identity.phrase,
    serverUrl,
  });
  return identity;
}

/**
 * Drives ChannelsList's "/join" form with a pre-generated invite code.
 * Assumes an already-authenticated page; gets to the channels list first via
 * the app's own "← /channels" back link if landed elsewhere.
 */
export async function joinChannelViaUi(page, inviteCode) {
  if (
    !(await page
      .getByRole('heading', { name: 'channels' })
      .isVisible()
      .catch(() => false))
  ) {
    const back = page.getByText('← /channels', { exact: true });
    if (await back.isVisible().catch(() => false)) await back.click();
  }
  await page.getByRole('heading', { name: 'channels' }).waitFor({ state: 'visible' });

  // Bottom command bar's "/join" toggle — the only "/join" text on screen
  // until the form below opens.
  await page.getByText('/join', { exact: true }).click();
  await page.getByLabel('Invite code', { exact: true }).fill(inviteCode);
  // The form's own submit button now also reads "/join" and renders before
  // the (still-present) toggle in DOM order — .first() is the submit
  // button, not the toggle (re-clicking the toggle would close the form).
  await page.getByText('/join', { exact: true }).first().click();
  await page.getByText('joined!', { exact: true }).waitFor({ state: 'visible', timeout: 10_000 });
}

/** Clicks a channel row by name and waits for the voice room to open. */
export async function enterChannelRoom(page, channelName) {
  await page.getByText(channelName, { exact: true }).click();
  await page.waitForURL(/\/room/, { timeout: 10_000 });
}

/**
 * Clicks "/join" on the default sub-room row in ParticipantsPanel.
 *
 * Per frontend_voice_room.md, being in a channel's voice call and being
 * "in a synchronized room" (sub-room) are distinct — `joinedSubRoomId` can
 * be null while fully connected, and ParticipantsPanel only ever renders
 * participants inside their joined sub-room. So nothing shows up there
 * (no participant count, no rows) until this explicit join happens — it is
 * NOT required for chat, which is channel-wide.
 *
 * Right after `enterChannelRoom`, ParticipantsPanel briefly (one paint,
 * confirmed via direct DOM polling) renders a second, differently-styled
 * "/join" element while room state is still hydrating, before settling on
 * the real single row. Clicking during that window trips Playwright's
 * strict-mode "resolved to 2 elements" check, so this waits for exactly one
 * stable match first.
 *
 * `expectedCount` is the participant count the sub-room header should settle
 * on afterward — 1 for the first (only) participant, 2+ when joining a room
 * that already has other real participants in it (e.g. a second live GUI
 * instance in the two-instances spec).
 */
export async function joinDefaultSubRoomViaUi(page, { expectedCount = 1 } = {}) {
  const joinButton = visibleText(page, '/join');
  const deadline = Date.now() + 10_000;
  while ((await joinButton.count()) !== 1 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  await joinButton.click();
  await visibleText(page, new RegExp(`ROOM \\d+\\s*\\(${expectedCount}\\)`)).waitFor({
    state: 'visible',
    timeout: 15_000,
  });
}

/**
 * Peer equivalent of joinDefaultSubRoomViaUi: waits for the server's
 * sub_room_state broadcast (sent automatically after join_voice, no
 * request needed), sends join_sub_room for the default room, and waits for
 * the server's sub_room_joined confirmation.
 */
export async function joinDefaultSubRoomAsPeer(peer) {
  const stateLine = await peer.waitForOutput(/"type":"sub_room_state"/);
  const parsed = JSON.parse(stateLine.replace(/^<<\s*/, ''));
  const rooms = parsed.rooms ?? [];
  const defaultRoom = rooms.find((r) => r.isDefault) ?? rooms[0];
  if (!defaultRoom) throw new Error('sub_room_state had no rooms to join');
  peer.send({ type: 'join_sub_room', subRoomId: defaultRoom.subRoomId });
  await peer.waitForOutput(/"type":"sub_room_joined"/);
}

/**
 * ActiveRoom renders three responsive layout tiers simultaneously (mobile
 * <md, intermediate md-1038px, desktop 1039px+ — see ActiveRoom.tsx) and
 * hides the inactive ones with CSS rather than unmounting them. LogsPanel
 * and ChatPanel each get instantiated once per tier that renders them
 * (desktop always; intermediate/mobile only when their own tab state
 * selects chat/log), so a plain placeholder/role locator can silently
 * resolve to a CSS-hidden duplicate instead of the one actually on screen.
 * `:visible` filters to whichever instance is really rendered.
 */
const VISIBLE_CLI_INPUT = '[data-cli-input="true"]:visible';
const VISIBLE_CHAT_INPUT = 'input[placeholder="type message..."]:visible';

/**
 * Types a command into ActiveRoom's LogsPanel CLI input and presses Enter,
 * switching to the LOG tab first if the CLI input isn't the visible tier's
 * default (desktop shows it by default; intermediate/mobile need the LOGS
 * tab selected first). Shared by leaveRoomIfActive and any spec driving
 * `/share`, `/stopshare`, etc. through the same input.
 */
export async function sendCliCommand(page, command) {
  let cli = page.locator(VISIBLE_CLI_INPUT).first();
  if (!(await cli.isVisible().catch(() => false))) {
    await page
      .locator('button:visible', { hasText: /^LOGS?\b/ })
      .first()
      .click();
    cli = page.locator(VISIBLE_CLI_INPUT).first();
  }
  await cli.fill(command);
  await cli.press('Enter');
}

/**
 * If the main window is currently in an active room, leaves it via the
 * room's `/leave` CLI command and waits for navigation away from /room.
 * No-op otherwise.
 *
 * Live-backend specs use this instead of test.skip()-ing on /room (the
 * pattern backend-independent specs like settings.spec.mjs use) because the
 * account in the isolated wavis-auth-e2e.json store is always this suite's
 * own throwaway session, never a real developer's live call — a previous
 * spec run (or the app restoring its last-open room on launch) can leave
 * the next run starting mid-room, and skipping forever there would make the
 * suite unrunnable a second time.
 */
export async function leaveRoomIfActive(page) {
  if (!new URL(page.url()).pathname.startsWith('/room')) return;
  await sendCliCommand(page, '/leave');
  await page.waitForURL((url) => !url.pathname.startsWith('/room'), { timeout: 10_000 });
}

/** Types and sends a chat message via ChatPanel, switching to its tab first if needed. */
export async function sendChatMessage(page, text) {
  let input = page.locator(VISIBLE_CHAT_INPUT).first();
  if (!(await input.isVisible().catch(() => false))) {
    await page
      .locator('button:visible', { hasText: /^CHAT\b/ })
      .first()
      .click();
    input = page.locator(VISIBLE_CHAT_INPUT).first();
  }
  await input.fill(text);
  await input.press('Enter');
}

/* ─── CLI peer (second signaling participant, no second GUI instance) ──── */

/**
 * Spawns `ws-sfu-test` in its interactive mode (raw JSON-line WebSocket
 * client — see scripts/ws-sfu-test/src/main.rs) as a scriptable second
 * participant. Chosen over `wavis-client`'s REPL because wavis-client has no
 * chat send/receive support at all (verified against its command parser);
 * ws-sfu-test lets the test send exact SignalingMessage JSON (auth,
 * join_voice, chat_send, ...) and read raw `<< {...}` echoes of whatever the
 * server sends back.
 *
 * This verifies the GUI's own rendering reacts to a second real signaling
 * participant — it does not exercise a second real GUI instance.
 */
export function spawnPeer() {
  const child = spawn('cargo', ['run', '-p', 'ws-sfu-test'], {
    cwd: WORKSPACE_ROOT,
    env: { ...process.env, WS_URL },
    stdio: ['pipe', 'pipe', 'pipe'],
  });

  const outputLines = [];
  const waiters = [];
  let buf = '';

  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    let idx;
    while ((idx = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, idx);
      buf = buf.slice(idx + 1);
      outputLines.push(line);
      for (let i = waiters.length - 1; i >= 0; i--) {
        if (waiters[i].pattern.test(line)) {
          waiters[i].resolve(line);
          waiters.splice(i, 1);
        }
      }
    }
  });

  function send(messageObj) {
    child.stdin.write(JSON.stringify(messageObj) + '\n');
  }

  function waitForOutput(pattern, timeoutMs = 15_000) {
    const existing = outputLines.find((line) => pattern.test(line));
    if (existing) return Promise.resolve(existing);
    return new Promise((resolve, reject) => {
      const entry = {
        pattern,
        resolve: (line) => {
          clearTimeout(timer);
          resolve(line);
        },
      };
      const timer = setTimeout(() => {
        const idx = waiters.indexOf(entry);
        if (idx >= 0) waiters.splice(idx, 1);
        reject(new Error(`Timed out waiting for peer output matching ${pattern}`));
      }, timeoutMs);
      waiters.push(entry);
    });
  }

  async function close() {
    try {
      child.stdin.end();
    } catch {
      // already closed
    }
    child.kill('SIGTERM');
  }

  return { send, waitForOutput, close, pid: child.pid };
}

/** auth -> join_voice against a channel, resolving once the server confirms both. */
export async function joinVoiceAsPeer(peer, { accessToken, channelId, displayName }) {
  peer.send({ type: 'auth', accessToken });
  await peer.waitForOutput(/"type":\s*"auth_success"/);
  peer.send({ type: 'join_voice', channelId, displayName });
  await peer.waitForOutput(/"type":\s*"joined"/);
}

/**
 * Matches a peer's raw `<< {...}` echo of a chat_message broadcast
 * containing the given text. Uses lookaheads rather than a single
 * ordered pattern because ChatMessagePayload's JSON fields serialize
 * alphabetically (displayName, messageId, participantId, text, timestamp,
 * type, userId) — "text" comes before "type" on the wire, not after.
 */
export function chatMessageReceivedPattern(text) {
  const escaped = text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`(?=.*"type":"chat_message")(?=.*"text":"${escaped}")`);
}

/**
 * Waits for the server's `media_token` message (sent automatically right
 * after `join_voice`/`Joined`, see sfu_relay.rs) and parses it as JSON.
 * Returns `{ token, sfuUrl }` — matches `MediaTokenPayload`'s wire shape
 * (`shared/src/signaling/mod.rs`: `token`, `#[serde(rename = "sfuUrl")]
 * sfu_url`).
 */
export async function waitForMediaToken(peer) {
  const line = await peer.waitForOutput(/"type":"media_token"/);
  const parsed = JSON.parse(line.replace(/^<<\s*/, ''));
  return { token: parsed.token, sfuUrl: parsed.sfuUrl };
}

/* ─── Network-condition simulation (LiveKit container netem) ───────────
 * CDP/Playwright network throttling does not touch WebRTC UDP, so media
 * degradation has to be injected at the LiveKit container's egress (its
 * eth0 — LiveKit -> GUI downstream is exactly the "incoming media" leg
 * these specs want to degrade). docker-compose.yml gives the livekit
 * service NET_ADMIN + installs iproute2 (tc) at container start.
 */
const LIVEKIT_CONTAINER = 'wavis-livekit';
const NETEM_INTERFACE = 'eth0';

/** True if `tc` is available in the livekit container — false means skip-with-reason, not fail. */
export async function hasNetemSupport() {
  try {
    await execFileAsync('docker', ['exec', LIVEKIT_CONTAINER, 'sh', '-c', 'command -v tc']);
    return true;
  } catch {
    return false;
  }
}

/** Shapes the livekit container's egress via netem. Any of delayMs/jitterMs/lossPct/rateKbit may be omitted. */
export async function setNetworkConditions({
  delayMs = 0,
  jitterMs = 0,
  lossPct = 0,
  rateKbit,
} = {}) {
  const args = [
    'exec',
    LIVEKIT_CONTAINER,
    'tc',
    'qdisc',
    'replace',
    'dev',
    NETEM_INTERFACE,
    'root',
    'netem',
  ];
  if (delayMs > 0) {
    args.push('delay', `${delayMs}ms`);
    if (jitterMs > 0) args.push(`${jitterMs}ms`);
  }
  if (lossPct > 0) args.push('loss', `${lossPct}%`);
  if (rateKbit) args.push('rate', `${rateKbit}kbit`);
  await execFileAsync('docker', args);
}

/** Removes netem shaping. Tolerates "no qdisc to delete" so it's safe to call unconditionally (e.g. in a finally). */
export async function clearNetworkConditions() {
  try {
    await execFileAsync('docker', [
      'exec',
      LIVEKIT_CONTAINER,
      'tc',
      'qdisc',
      'del',
      'dev',
      NETEM_INTERFACE,
      'root',
    ]);
  } catch {
    // No qdisc present — already clear.
  }
}

/* ─── Backend outage simulation (wavis-backend container) ──────────────
 * Unlike the netem helpers above (which degrade the LiveKit media leg),
 * these fully stop the signaling/REST backend to simulate a real "server
 * down" outage. `docker stop` (not `pause`) is deliberate: `pause` freezes
 * the process via the cgroup freezer but leaves existing sockets open and
 * silent, so the GUI's WebSocket would never see a close event and could
 * hang far longer than intended; `stop` tears the process down, so the OS
 * immediately refuses/resets connections the way a genuinely dead backend
 * would.
 */
const BACKEND_CONTAINER = 'wavis-backend';

/** Stops the backend container. Used by zz-disconnect-sound.spec.mjs. */
export async function stopBackendContainer() {
  await execFileAsync('docker', ['stop', BACKEND_CONTAINER]);
}

/**
 * Starts the backend container and waits for /health to respond again.
 * Safe to call unconditionally (e.g. in a finally) — `docker start` on an
 * already-running container is a no-op. Used by zz-disconnect-sound.spec.mjs.
 */
export async function startBackendContainer(healthTimeoutMs = 30_000) {
  try {
    await execFileAsync('docker', ['start', BACKEND_CONTAINER]);
  } catch {
    // Already running.
  }
  await waitForBackendHealth(healthTimeoutMs);
}

/**
 * Restarts the backend container (stop + start) and waits for /health to
 * report ok again — used by reconnect.spec.mjs. Composed from the two
 * primitives above rather than `docker restart`, so both specs share one
 * "server down" mechanism (a hard stop, not a pause) instead of two subtly
 * different ones. Dropping the connection this way takes down every WS
 * connection to the backend (both the GUI's signaling socket and any
 * spawnPeer()/ws-sfu-test connection — ws-sfu-test is a one-shot
 * connect_async with no reconnect loop of its own, confirmed via its
 * source, so a peer connection does not survive this and must be
 * re-established by the caller if the scenario needs a peer present both
 * before and after). wavis-livekit is untouched, so an already-negotiated
 * LiveKit media session (e.g. a connectTonePeer() publish) is not expected
 * to drop on its own.
 */
export async function restartBackend({ healthTimeoutMs = 60_000 } = {}) {
  await stopBackendContainer();
  await startBackendContainer(healthTimeoutMs);
}
