// Live-backend, TWO REAL Wavis GUI instances at once (not spawnPeer's
// signaling-only stand-in — see the `appB` fixture in fixtures.mjs and
// README.md's "Two simultaneous GUI instances" section).
//
// Two things are proven here:
// 1. Two different accounts can each drive a real GUI instance into the
//    SAME voice room simultaneously with no displacement — the "another
//    window" error (session_displaced) is keyed by account user_id
//    server-side (voice_orchestrator.rs's evict_stale_session), not by
//    device or window, so this only works because the two instances log in
//    as two DIFFERENT accounts.
// 2. The inverse: the SAME account joining the same voice room a second
//    time (here via a spawnPeer() authenticating with the first session's
//    own access token) genuinely displaces the first — this is a server
//    design constraint, not a harness bug, and is worth documenting
//    alongside (1) so nobody "fixes" two-instance flakiness by chasing it.
//
// Both instances launch from the SAME debug exe (build once, per README),
// isolated from each other via driver.mjs's per-launch authStoreName /
// keyringService options — see main.rs's get_auth_store_name /
// keyring_service and auth.ts's resolveStoreName.
import { test, expect } from './fixtures.mjs';
import {
  SERVER_URL,
  waitForBackendHealth,
  seedChannelWithInvite,
  registerAndLoginViaUi,
  joinChannelViaUi,
  enterChannelRoom,
  leaveRoomIfActive,
  joinDefaultSubRoomViaUi,
  joinDefaultSubRoomAsPeer,
  visibleText,
  participantRow,
  spawnPeer,
  joinVoiceAsPeer,
  registerDevice,
  loginViaUi,
} from './live-backend-helpers.mjs';

test(
  'two accounts in two real GUI instances join the same voice room with no displacement',
  async ({ app, appB }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-two-instances-${suffix}`;
    const { invite } = await seedChannelWithInvite(channelName);

    const main = await app.page();
    const other = await appB.page();
    await leaveRoomIfActive(main);
    await leaveRoomIfActive(other);

    const identityA = await registerAndLoginViaUi(main, { serverUrl: SERVER_URL });
    const identityB = await registerAndLoginViaUi(other, { serverUrl: SERVER_URL });
    expect(identityA.user_id).not.toBe(identityB.user_id);

    await joinChannelViaUi(main, invite.code);
    await joinChannelViaUi(other, invite.code);

    await enterChannelRoom(main, channelName);
    await enterChannelRoom(other, channelName);

    await joinDefaultSubRoomViaUi(main, { expectedCount: 1 });
    await joinDefaultSubRoomViaUi(other, { expectedCount: 2 });

    // Both windows converge on the same 2-participant room state, each
    // showing the OTHER account's username — real cross-instance visibility,
    // not each window just seeing itself.
    await expect(visibleText(main, /ROOM \d+\s*\(2\)/)).toBeVisible({ timeout: 10_000 });
    await expect(participantRow(main, identityB.username)).toBeVisible();
    await expect(participantRow(other, identityA.username)).toBeVisible();

    // The core claim under test: neither instance was evicted by the other.
    await expect(main.getByText('Session taken over by another device')).toHaveCount(0);
    await expect(other.getByText('Session taken over by another device')).toHaveCount(0);
    await expect(main).toHaveURL(/\/room/);
    await expect(other).toHaveURL(/\/room/);
  },
  { timeoutMs: 180_000 },
);

test(
  'the SAME account joining the same voice room a second time IS displaced',
  async ({ app }) => {
    await waitForBackendHealth();

    const suffix = Date.now().toString(36);
    const channelName = `e2e-displacement-${suffix}`;
    const { channel, invite } = await seedChannelWithInvite(channelName);

    const main = await app.page();
    await leaveRoomIfActive(main);

    // registerDevice() (REST) mints the account's first session; loginViaUi
    // then authenticates the SAME account a second time (its own device
    // session) into the GUI — mirroring how a real second window would log
    // in, without needing a second real GUI instance for this assertion.
    const identity = await registerDevice();
    await loginViaUi(main, {
      recoveryId: identity.recovery_id,
      password: identity.phrase,
      serverUrl: SERVER_URL,
    });

    await joinChannelViaUi(main, invite.code);
    await enterChannelRoom(main, channelName);
    await joinDefaultSubRoomViaUi(main, { expectedCount: 1 });

    // Same account (identity.access_token), same channel -> server-side
    // evict_stale_session evicts by user_id, displacing the GUI's session.
    const peer = await spawnPeer({ accessToken: identity.access_token });
    try {
      await joinVoiceAsPeer(peer, {
        channelId: channel.channel_id,
        displayName: 'DisplacerBot',
      });
      await joinDefaultSubRoomAsPeer(peer);

      await expect(main.getByText('Session taken over by another device')).toBeVisible({
        timeout: 15_000,
      });
    } finally {
      await peer.close();
    }
  },
  { timeoutMs: 60_000 },
);
