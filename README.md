# Wavis
Native real-time voice for small private groups. Max 6 participants, invite-only rooms, no browser required.

Wavis is a cross-platform desktop app (Windows, macOS, Linux) built with Tauri 2.0 + React and a Rust signaling backend. It supports 1:1 P2P voice, SFU multi-party voice, screen sharing with system audio, in-room chat, and a credential-free device identity system.

## What's Shipped

- **P2P voice** — 1:1 WebRTC audio with Opus, noise suppression (nnoiseless), adaptive bitrate, jitter buffer
- **SFU multi-party voice** — 2–6 participants via LiveKit; room type auto-detected from config
- **Screen sharing** — multiple concurrent shares per room, video + system audio slots, platform-native capture (Windows: Graphics Capture API + WASAPI; macOS: native + WavisAudioTap HAL; Linux: PipeWire/X11); VP8 simulcast
- **In-room chat** — ephemeral relay with 24-hour persistence and history replay
- **Device identity** — credential-free registration; phrase-based recovery; QR/code device pairing; multi-device support; session epoch for atomic logout-all
- **Channel system** — channel CRUD, membership roles (Owner/Admin/Member), invite codes (expiry, max-use, revocation), bans, channel-scoped voice rooms
- **Desktop GUI** — Tauri 2.0 + React 19, voice room state machine with reconnection, participant list with speaking indicators, Watch All grid, global mute hotkey, settings (volume, codec, noise suppression, hotkeys), in-app bug reporting with diagnostics window
- **Security** — per-IP and per-connection rate limiting, temporary IP bans, TURN credential generation, JWT with reuse detection, Argon2id phrase hashing, AES-256-GCM phrase encryption, DTLS-SRTP transport encryption, 22 enforced security invariants

## Architecture

```
┌──────────────────────┐      WebSocket      ┌──────────────────────┐
│  Desktop App         │◄───────────────────►│   Wavis Backend      │
│  (Tauri 2.0 + React) │                     │   (Control Plane)    │
└──────────┬───────────┘                     └──────────┬───────────┘
           │                                            │ LiveKit API
           │  WebRTC                                    ▼ (SFU mode)
           │  P2P ──────────────────────────────► ┌──────────────┐
           │                                      │  LiveKit SFU │
           └──────────────────────────────────────►│  (media)     │
                                                  └──────────────┘
```

The backend is the **control plane only**: room lifecycle, channel membership, invite validation, capacity enforcement, JWT issuance, signaling relay, TURN credentials. It never touches media. In P2P mode, media flows directly between clients. In SFU mode, media flows through LiveKit.

The CLI test client (`clients/cli-test/`) speaks the same WebSocket API and is used for dev and integration testing.

## Project Structure

| Directory | Description |
|-----------|-------------|
| `wavis-backend/` | Control plane server (Axum, WebSocket, room state) |
| `clients/wavis-gui/` | Desktop app (Tauri 2.0 + React) |
| `clients/shared/` | Shared client library (WebRTC, audio pipeline, signaling) |
| `clients/cli-test/` | CLI test client for dev and integration testing |
| `shared/` | Signaling protocol types (shared between server and clients) |
| `infrastructure/` | Terraform / deployment config |
| `tools/` | Stress tests, GUI surface tests |
| `scripts/` | Dev utilities |
| `doc/` | Quickstart, testing docs, deployment guides |

## Getting Started

**Prerequisites:** Rust toolchain, Node.js LTS, [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.

### Backend

```powershell
Copy-Item .env.example .env
cargo run -p wavis-backend
```

### Desktop App

```powershell
cd clients/wavis-gui
npm install
npx tauri dev
```

First run compiles the Rust shell — expect a few minutes.

### Loopback Test (no server needed)

```powershell
cargo run -p wavis-cli-test -- --loopback
```

Runs mic → WebRTC → speakers locally to verify audio before connecting to a backend.

For full setup including Docker, LiveKit, TURN, and multi-terminal P2P/SFU tests, see [doc/QUICKSTART.md](doc/QUICKSTART.md).

## Channels and Invites

Rooms are scoped to channels. The host creates a channel, generates an invite code, and shares it out-of-band. The backend validates the code, enforces capacity (max 6), and issues a JWT for media access. Codes support expiration, max-use limits, and revocation. When the last peer leaves the channel room, it is cleaned up automatically.

Set `REQUIRE_INVITE_CODE=false` to bypass invite validation in local development.

## Identity

Wavis uses closed-alpha registration: `POST /auth/register` requires an
`invite_code` / `inviteCode` value before the backend creates a user, device,
or tokens. Alpha invite codes are stored only as HMAC-SHA256 hashes in Postgres
and track expiry, disabled state, and redemption count. The legacy
`POST /auth/register_device` endpoint is retired and returns `410 Gone`.

Each account has a recovery phrase (Argon2id, AES-256-GCM encrypted at rest).
Additional devices can be paired via QR or a short code. Recovery on a new
device requires only the recovery phrase. Refresh tokens use HMAC-SHA256 hashing
with reuse detection and session epoch for atomic logout-all.

## Voice Modes

| Mode | Participants | When |
|------|-------------|------|
| P2P | 2 | `LIVEKIT_*` env vars not set |
| SFU | 2–6 | `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `LIVEKIT_HOST` all set |

The backend auto-selects the mode at startup. The `livekit` feature flag on `clients/shared` gates the LiveKit Rust SDK dependency.

## Security

- DTLS-SRTP transport encryption (WebRTC standard); no E2EE (in SFU mode, LiveKit can access media)
- Invite-only rooms; invite codes enforce expiration, max-use, and revocation
- Server-enforced capacity (max 6); host can kick participants
- Mute is advisory (client-enforced), not media-level
- Per-IP and per-connection rate limiting; temporary IP bans for abuse
- Opaque error responses; no sensitive field leakage; `no_sensitive_logs` integration test enforces this
- 22 security invariants enforced — see `.kiro/steering/security.md`

## Testing

```powershell
cargo test --workspace
```

All automated tests use mocks — no running server, no audio hardware, no LiveKit needed.

```powershell
cargo clippy --workspace -- -D warnings
```

For manual test walkthroughs (invite lifecycle, rate limiting, kick, SDP size limits, room cleanup), see [doc/testing/](doc/testing/).

### LiveKit E2E Tests

These run the real `LiveKitSfuBridge` against a live LiveKit server. They verify room lifecycle, media token issuance, JWT validity, and room cleanup.

**1. Start LiveKit and Redis:**

```powershell
docker compose up -d redis livekit
```

**2. Run the tests:**

```powershell
$env:LIVEKIT_API_KEY="devkey"; $env:LIVEKIT_API_SECRET="secret"; $env:LIVEKIT_HOST="ws://localhost:7880"; $env:SFU_JWT_SECRET="dev-secret-32-bytes-minimum!!!XX"; cargo test -p wavis-backend --test livekit_e2e_integration -- --ignored --test-threads=1
```

Credentials are the dev defaults from `docker-compose.yml`. `--test-threads=1` prevents room name collisions.

**3. Tear down:**

```powershell
docker compose down redis livekit
```

These tests also run in CI via the `LiveKit E2E` GitHub Actions workflow on PRs touching `livekit_bridge.rs`, `sfu_relay.rs`, `livekit.yaml`, or `docker-compose.yml`.

## Docs

- [doc/QUICKSTART.md](doc/QUICKSTART.md) — commands and runbooks
- [doc/TESTING.md](doc/TESTING.md) — test strategy and manual test walkthroughs
- [doc/turn_credentials_audit.md](doc/turn_credentials_audit.md) — TURN topology, investigation steps, rollback procedure

## Contributing

Follow the layering rules:

- **Handlers** — transport only (WebSocket / HTTP)
- **Domain** — business logic (channels, invites, JWT, capacity, screen share)
- **State** — in-memory storage and concurrency

Don't duplicate WebRTC, signaling, or permission logic. Shared types live in `shared/`. Check `.kiro/steering/` for area-specific architectural guidance before touching any subsystem.

Security-sensitive logic (invite validation, rate limiting, JWT, phrase handling) requires property tests. Don't log sensitive fields — the `no_sensitive_logs` integration test enforces this at CI time.

New `SignalingMessage` variants require updates in: `mod.rs`, `validation.rs`, `proptest_support.rs`, `ws.rs`, `call_session.rs`. New Tauri window labels must be added to `capabilities/default.json`.

Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` before pushing.

## Why Rust?

Memory safety without a GC, predictable latency, strong async ecosystem, and a single language across backend and native clients. No runtime overhead, no hidden allocations.
s a `no_sensitive_logs` integration test that enforces this.

Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` before pushing.
s a `no_sensitive_logs` integration test that enforces this.

Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` before pushing.

## License

MIT — see [LICENSE](LICENSE).
