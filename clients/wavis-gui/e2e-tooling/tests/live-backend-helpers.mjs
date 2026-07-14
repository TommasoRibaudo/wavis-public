// Shared helpers for the live-backend specs (login/room-join/chat/participants
// .spec.mjs). These need a reachable backend — see e2e-tooling/README.md's
// "Live-backend specs" section for the local docker-compose runbook.
//
// Not used by the backend-independent specs (launch/title-bar/multi-window/
// settings) — those stay dependency-free on purpose.
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

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
 * POST /auth/register_device — mints a full session with no UI/phrase step.
 * Use this for identities that never need to touch the GUI (e.g. a channel
 * owner in room-join.spec.mjs), not as a substitute for exercising the real
 * auth UI (that's what registerViaUi / login.spec.mjs are for).
 */
export async function registerDevice() {
  const res = await fetch(`${SERVER_URL}/auth/register_device`, { method: 'POST' });
  if (!res.ok) throw new Error(`register_device failed: ${res.status}`);
  return res.json(); // { user_id, device_id, recovery_id, access_token, refresh_token }
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

/** REST-seeds a channel + invite owned by a fresh register_device identity. */
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
 */
export async function registerViaUi(page, { username, password, serverUrl = SERVER_URL } = {}) {
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
 */
export async function joinDefaultSubRoomViaUi(page) {
  const joinButton = visibleText(page, '/join');
  const deadline = Date.now() + 10_000;
  while ((await joinButton.count()) !== 1 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  await joinButton.click();
  await visibleText(page, /ROOM \d+\s*\(1\)/).waitFor({ state: 'visible', timeout: 15_000 });
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
