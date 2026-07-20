# Closed-Alpha Release Smoke Test

A release-gate checklist for the closed-alpha access path (#239 / #269):
invite redemption -> token issuance -> WebSocket ticket issuance -> room
connection, and the unauthorized/replay/double-redemption boundaries around
it. Run this before promoting a build that touches `wavis-backend/src/auth/`
or `wavis-backend/src/ws/` to the closed-alpha environment.

See `docs/alpha-invite-operations.md` for how to create/list/disable
individual invites with `alpha-invite-admin` — this runbook assumes that tool
is already available and `DATABASE_URL`/`ALPHA_INVITE_CODE_PEPPER` are set to
the target environment's values.

## 1. Automated coverage (run first)

```powershell
cargo test -p wavis-backend --test closed_alpha_e2e_integration --features test-support -- --ignored --test-threads=1
cargo test -p wavis-backend --test auth_integration --features test-support -- --ignored --test-threads=1
cargo test -p wavis-backend --test ws_ticket_gate_integration --features test-support -- --test-threads=1
cargo test -p wavis-backend --test ws_ticket_rest_integration --features test-support -- --ignored --test-threads=1
```

All four must pass against a real Postgres instance (`DATABASE_URL`) before
continuing to the manual pass below. Together they cover:

| Requirement | Test |
|---|---|
| Invite redemption -> tokens -> WS ticket -> room connection | `closed_alpha_e2e_integration::full_closed_alpha_path_invite_to_room_connection` |
| Registered user can refresh and reconnect | `closed_alpha_e2e_integration::registered_alpha_user_can_refresh_and_reconnect` |
| Non-alpha user cannot register | `closed_alpha_e2e_integration::non_alpha_user_cannot_create_an_account` |
| Cannot refresh without a valid token | `closed_alpha_e2e_integration::unauthorized_clients_cannot_refresh_without_a_valid_token` |
| Cannot open a WebSocket without a ticket | `closed_alpha_e2e_integration::unauthorized_clients_cannot_open_websockets_on_the_full_route_surface` |
| One invite cannot be redeemed twice (max_redemptions=1) | `closed_alpha_e2e_integration::single_use_invite_cannot_be_redeemed_twice_over_http`, `auth_integration::alpha_invite_registration_exhausts_limited_invite` |
| Concurrent redemption admits exactly the configured count | `auth_integration::concurrent_redemption_of_last_slot_admits_exactly_one` |
| Consumed WS ticket cannot be replayed | `closed_alpha_e2e_integration::consumed_ws_ticket_cannot_be_replayed_over_http`, `ws_ticket_gate_integration::ws_with_valid_ticket_succeeds_once_then_replay_fails` |
| `register_device` (legacy) is gone | `auth/routes.rs::register_device_returns_gone_without_consuming_register_limit` |

## 2. Manual pass against a real deployment

Run these against the actual server URL you're about to release (not just
local docker) — the automated suite above proves the code is correct, this
proves the deployed config (peppers, `REQUIRE_INVITE_CODE`, TLS) is correct
too.

1. **Legacy endpoint is dead.**
   ```powershell
   curl.exe -s -o NUL -w "%{http_code}" -X POST https://<host>/auth/register_device
   ```
   Expect `410`.

2. **Registration requires a real invite.**
   ```powershell
   curl.exe -s -o NUL -w "%{http_code}" -X POST https://<host>/auth/register -H "Content-Type: application/json" -d '{\"phrase\":\"smoke-test-phrase\",\"username\":\"smoke-test\"}'
   ```
   Expect `401` (no `invite_code`).

3. **Create a single-use invite** with `alpha-invite-admin create --max-redemptions 1`,
   note the printed code, then redeem it:
   ```powershell
   curl.exe -s -X POST https://<host>/auth/register -H "Content-Type: application/json" -d '{\"phrase\":\"smoke-test-phrase-1\",\"username\":\"smoke-test-1\",\"invite_code\":\"<CODE>\"}'
   ```
   Expect `201` with `access_token`/`refresh_token`/`recovery_id`. Save
   `access_token` and `refresh_token`.

4. **The same invite cannot be redeemed again:**
   ```powershell
   curl.exe -s -o NUL -w "%{http_code}" -X POST https://<host>/auth/register -H "Content-Type: application/json" -d '{\"phrase\":\"smoke-test-phrase-2\",\"username\":\"smoke-test-2\",\"invite_code\":\"<CODE>\"}'
   ```
   Expect `401`.

5. **Mint a WS ticket and connect:**
   ```powershell
   curl.exe -s -X POST https://<host>/auth/ws-ticket -H "Authorization: Bearer <access_token>"
   ```
   Expect `200` with a `ticket` and positive `expires_in`. Then confirm a
   WebSocket client can connect to `wss://<host>/ws?ticket=<ticket>` and
   join a room, and that connecting to `/ws` **without** `?ticket=` is
   rejected before upgrade.

6. **The same ticket cannot be replayed** — reconnect to the same
   `wss://<host>/ws?ticket=<ticket>` URL a second time; the second attempt
   must fail before upgrade.

7. **Refresh and reconnect:**
   ```powershell
   curl.exe -s -X POST https://<host>/auth/refresh -H "Content-Type: application/json" -d '{\"refresh_token\":\"<refresh_token>\"}'
   ```
   Expect `200` with a new `access_token`/`refresh_token` pair (different
   from step 3's). Mint a fresh WS ticket with the new access token and
   confirm it connects. Then confirm the **original** `refresh_token` from
   step 3 is now rejected (`401`) — rotation must invalidate it.

8. **Disable the smoke-test invite** (`alpha-invite-admin disable --id <uuid>`)
   so it can't be redeemed again, and record the smoke-test accounts created
   in steps 3–4 for cleanup if the target environment isn't ephemeral.

## 3. GUI registration path (optional, when `DeviceSetup` changed)

`clients/wavis-gui/e2e-tooling`'s `registerViaUi` drives the real
`DeviceSetup` registration form, including its invite-code field, against a
live backend — see that package's README for how to build a debug exe with
`VITE_ALLOW_INSECURE_TLS=true` and launch it. Useful when this release also
changes the registration UI, not just the backend.
