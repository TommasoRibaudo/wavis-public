# Wavis

Native real-time voice for small private groups. Max 6 participants, invite-only rooms, no browser required.

Built with a Rust control-plane backend and a Tauri 2.0 + React desktop client. P2P voice is stable; SFU multi-party is in active development.

Wavis favors simplicity, native performance, and explicit room control over feature breadth.

## Architecture

┌──────────────────────┐      WebSocket      ┌─────────────
│  Desktop App         │◄───────────────────►│  Wavis Backend   │
│  (Tauri 2.0   │  (ControlPlane) │
└──────────┬──└────────┬─────────┘
           │  WebRTC (P2P or SFU)                     │ LiveKit API
           │                                          ▼
(optional)
       ┌───▼────┐                             ┌──────────────┐
       │  Peer──►│  LiveKit
✻ Brewed for 4m 19s

> I do not see your changes... just paste here the entire    readme, I will

# Wavis

Native real-time voice for small private groups. Max 6 participants, invite-only rooms, no browser required.

Built with a Rust control-plane backend and a Tauri 2.0 + React desktop client. P2P voice is stable; SFU multi-party is in active d
                                             Wavis favors s, and explicitroom control over feature breadth.

## Architecture

┌──────────────────────┐      WebSocket      ┌─────────────
│  Desktop App         │◄───────────────────►│  Wavis Backend   │
│  (Tauri 2.0   │  (ControlPlane) │
└──────────┬───────────┘
└────────┬────
           │  WebRTC (P2P or SFU)                     │ LiveKit API
           │             ▼(optional)
       ┌───▼────┐                             ┌──────────────┐
       │  Peer──►│  LiveKit
─────┘                             └──────────────┘

The backend is the control plane: room lifecycle, channels, invite codes, join validation, capacity enforcement, JWT issuance, and WebSocket signaling. It never processes media. In SFU mode, media goes through LiveKit.

The CLI test client (`clients/cli-test/`) connects over the
same WebSocketintegrationtesting.

## Project Structure

 | Description |
|-----------|-------------|
| `wavis-backend/` | Control plane server (Axum, WebSocket, room state) |
| `clients/wavis-gui/` | Desktop app (Tauri 2.0 + React) |
| `clients/shared/` | Shared client library (WebRTC, audio pipeline, signaling) |
| `clients/cli-test/` | CLI test client for dev and integration testing |
| `shared/` | red betweenserver and clients) |
| `infrastructure/` | Terraform / deployment config |      | `tools/` | Ss |
| `scripts/` | Dev utilities |
| `doc/` | Quickstart, testing docs, deployment guides |

## Getting Started

**Prerequisites:** Rust toolchain, Node.js LTS, and [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.

### Backend

```powershell
Copy-Item .env.example .env
cargo run -p wavis-backend

Desktop App

cd clients/wavis-gui
npm installnpx tauri dev

First run compiles the Rust shell — expect a few minutes.
                                                    Loopback Testal P2P/SFU tests, see doc/QUICKSTART.md.

Channels and Invites
                                                    Rooms are orgareates achannel, generes itout-of-band. The backend validates the code, enforces capacity (max 6), and issues a JWT for media access. Codes support max-use limits, expiration, and revocation. When the last peer p automatically. accounts. Devices register with a generated recovery phrase (hashed server-side with Argon2id). Additional devices can be paired via QR or a short code. Recovering on a new device requires only the recovery    phrase. No pas

- Phase 1 ✅ — control plane (rooms, invites, signaling, JWT)
- Phase 2 ✅ — device auth, channel-based rooms, P2P 1:1 voice
- Phase 3 (active) — SFU multi-party voice, screen sharing
- Phase 4 (planned) — E2EE, push notifications, optional full accounts
                                                        Security Model

- Transport encryption via DTLS-SRTP (WebRTC standard)
- No end-to-end encryption — in SFU mode, LiveKit can access media                                            - Invite-only e limits, and
--workspace

All automated tests use mocks — no running server, no audio hardware, no LiveKit instance needed.

For manual test walkthroughs (invite lifecycle, rate limiting, kick moderation, SDP size limits, room cleanup), see doc/testing/.                                       
LiveKit E2E Tests
                                                        These run the  a live LiveKitserver. They verify room lifecycle, media token issuance, JWT validity, and room cleanup — things mock-based tests cannot cover.

1. Start LiveKit and Redis:                             -p wavis-backend --test livekit_e2e_integration -- --ignored --test-threads=1

The credential values are the dev defaults from docker-compose.yml. --test-threads=1 prevents room name collisions between tests.

3. Tear down:                                           
docker compose down redis livekit

These tests also run in CI via the LiveKit E2E GitHub   Actions workflidge.rs,
credentials_audit.md — TURN topology, investigation steps, rollback procedure

Contributing

Follow the layering rules:

- Handlers: transport only (WebSocket / HTTP)           - Domain: busiWT, capacity)
- State: in-memory storage and concurrency

Don't duplicate WebRTC, signaling, or permission logic. Shared types lteering/ forvite codes, peer IPs) — the test suite has a no_sensitive_logs integration test that enforces this.

Run cargo test --workspace and cargo clippy --workspace -- -D warnings before pushing.

Why Rust?

Memory safety ency, strongasync ecosystem, and a single language across backend and native clients. No runtime overhead, no hidden allocations.

License

MIT — see LICENSE.
