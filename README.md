# Wavis

Native real-time voice for small private groups. Invite-only rooms, max 6 participants, no browser required.

Clients are native Rust applications (desktop/mobile targets planned).

🚧 Status: Actively developed. P2P voice stable. SFU multi-party in progress.

Wavis favors simplicity, native performance, and explicit room control over feature breadth.

## Architecture

```
┌─────────────┐       WebSocket        ┌──────────────────┐
│  CLI Client │◄──────────────────────►│  Wavis Backend   │
│  (Rust)     │                        │  (Control Plane) │
└──────┬──────┘                        └────────┬─────────┘
       │                                        │
       │  WebRTC (P2P or SFU)                   │ LiveKit API
       │                                        │ (optional)
       ▼                                        ▼
   ┌────────┐                           ┌──────────────┐
   │  Peer  │◄─────────────────────────►│  LiveKit SFU │
   └────────┘                           └──────────────┘
```

The backend is the control plane: room lifecycle, invite codes, join validation, capacity enforcement, JWT issuance, and WebSocket signaling. It never processes media. In SFU mode, media is handled by LiveKit.

Media flows directly between peers (P2P) or through LiveKit (SFU mode). Rooms are intentionally capped at 6 participants to maintain low latency and simplicity.

## Project Structure

```
wavis-backend/     Control plane server (Axum, WebSocket, room state)
clients/shared/    Shared client library (WebRTC, audio pipeline, signaling)
clients/cli-test/  CLI test client for manual and integration testing
shared/            Signaling protocol types (shared between server + client)
scripts/           Dev utilities (ws-sfu-test, PowerShell WS script)
deploy/            LiveKit deployment config
doc/               Detailed docs (quickstart, testing, QA reports)
```

## Quick Start

```bash
cp .env.example .env
cargo run -p wavis-backend
```

In another terminal:

```bash
cargo run -p wavis-cli-test -- --loopback
```

That runs a local audio loopback test (mic → WebRTC → speakers, no server needed).

For the full setup guide including Docker, LiveKit, and multi-terminal P2P/SFU tests, see [doc/QUICKSTART.md](doc/QUICKSTART.md).

## Modes

| Mode                 | Description                                                    |
|----------------------|----------------------------------------------------------------|
| Default (P2P)        | Direct WebRTC 1:1 voice between two peers (no server media)    |
| `--features livekit` | Multi-party voice via LiveKit SFU (2–6 participants)           |

The `livekit` feature flag applies to `clients/shared` only and gates the LiveKit Rust SDK dependency. The backend auto-detects LiveKit mode when `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, and `LIVEKIT_HOST` are all set; otherwise it uses the built-in mock SFU bridge.

## How Invites Work

Host creates a room → generates an invite code → shares it out-of-band → others join with the code. The backend validates the code, enforces capacity (max 6), and issues a JWT for media access. Codes support max-use limits, expiration, and revocation. When the last peer leaves, the room and its invite codes are cleaned up automatically.

For local development, set `REQUIRE_INVITE_CODE=false` to bypass invite validation.

## Development Phases

- Phase 1 (done): Control plane — room lifecycle, invite codes, signaling, JWT issuance
- Phase 2 (current): P2P 1:1 voice via WebRTC
- Phase 3 (planned): SFU multi-party voice, screen sharing

## Security Model (MVP)

- Transport encryption via DTLS-SRTP (WebRTC standard)
- No end-to-end encryption — in SFU mode, LiveKit can access media
- Invite-only rooms with expiration, max-use limits, and revocation
- Server-enforced capacity (max 6) and role-based moderation (host can kick)
- Mute is advisory (client-enforced), not media-level enforced
- The backend enforces per-IP rate limits and temporary bans to prevent invite brute-forcing and connection abuse

## Testing

```bash
cargo test --workspace
```

All automated tests use mocks — no running server, no audio hardware, no LiveKit instance needed.

For manual test walkthroughs (invite lifecycle, rate limiting, kick moderation, SDP size limits, room cleanup), see [doc/testing/](doc/testing/).

### LiveKit E2E Tests

These tests run the real `LiveKitSfuBridge` against a running LiveKit server. They verify room lifecycle, media token issuance, JWT validity, and room cleanup — things the mock-based tests cannot cover.

**1. Start LiveKit and Redis:**

```bash
docker compose up -d redis livekit
```

Wait a few seconds for LiveKit to be fully ready before running the tests.

**2. Run the tests:**

```bash
LIVEKIT_API_KEY=devkey \
LIVEKIT_API_SECRET=secret \
LIVEKIT_HOST=ws://localhost:7880 \
SFU_JWT_SECRET=dev-secret-32-bytes-minimum!!!XX \
cargo test -p wavis-backend --test livekit_e2e_integration -- --ignored --test-threads=1
```

The credential values above are the dev defaults from `docker-compose.yml`. `--test-threads=1` prevents room name collisions between tests.

**3. Tear down when done:**

```bash
docker compose down redis livekit
```

These tests also run automatically in CI via the `LiveKit E2E` GitHub Actions workflow on PRs that touch `livekit_bridge.rs`, `sfu_relay.rs`, `livekit.yaml`, or `docker-compose.yml`.

## Docs

- [doc/QUICKSTART.md](doc/QUICKSTART.md) — commands and runbooks
- [doc/testing/](doc/testing/) — test strategy, manual tests, AI runner notes
- [doc/deployment-strategy.md](doc/deployment-strategy.md) — local / dev / prod deployment
- [doc/ci-cd-pipeline.md](doc/ci-cd-pipeline.md) — GitHub Actions CI/CD

## Why Rust?

Memory safety without a GC, predictable latency, strong async ecosystem, and a single language across backend and native clients. No runtime overhead, no hidden allocations.

## Contributing

Follow the layering rules:

- Handlers: transport only (WebSocket / HTTP)
- Domain: business logic (rooms, invites, JWT, capacity)
- State: in-memory storage and concurrency

Don't duplicate WebRTC, signaling, or permission logic. Shared types live in `shared/`. Check `.kiro/steering/` for area-specific architectural guidance.

Security-sensitive logic (invite validation, rate limiting, JWT) requires property tests. Don't log sensitive fields (tokens, invite codes, peer IPs) — the test suite has a `no_sensitive_logs` integration test that enforces this.

Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` before pushing.

## License

MIT — see [LICENSE](LICENSE).
