# Wavis

Native real-time voice for small private groups. Max 6 participants, invite-only rooms, no browser required.

Wavis is a cross-platform desktop app (Windows, macOS, Linux) built with Tauri 2.0 + React and a Rust signaling backend. It supports 1:1 P2P voice, SFU multi-party voice, screen sharing with system audio, in-room chat, and a credential-free device identity system.

## What's Shipped

- **P2P voice*s, noisesuppression (nnoiseless), adaptive bitrate, jitter buffer
- **SFU multi-party voice** — 2–6 participants via LiveKit; room type auto

Wavis is a cross-platform desktop app (Windows, macOS, Linux) built wust signalingbackend. It supports 1:1 P2P voice, SFU multi-party voice, screen sharing with system audio, in-room chat, and a credential-free device identity system.              
## What's Shipped

- **P2P voice** — 1:1 WebRTC audio with Opus, noise suppression (nnoiseless), adaptive bitrate, jitter buffer
- **SFU multi-party voice** — 2–6 participants via LiveKit; room type auto-detected from config
- **Screen sharing** — multiple concurrent shares per room, video + system audio slots, platform-native capture  (Windows: GrapcOS: native +WavisAudioTap HAL; Linux: PipeWire/X11); VP8 simulcast
- **In-room chat** — ephemeral relay with 24-hour    persistence an
- **Device identity** — credential-free registration; phrase-based recovery; QR/code device pairing; multi-device support; session epoch for atomic logout-all
- **Channel system** — channel CRUD, membership roles (Owner/Admin/Member), invite codes (expiry, max-use, revocation), bms
- **Desktop GUI** — Tauri 2.0 + React 19, voice room state machine with reconnection, participant list with indicators, Wakey, settings(volume, codec, in-app bugreporting with diagnostics window
- **Security** — per-IP and per-connection rate limiting, temporary IP bans, TURN credential generation, JWreuse detectioES-256-GCM

## Architecture

┌──────────────────────┐      WebSocket      ┌──────────────────────┐
│  Desktop App         │◄───────────────────►│   Wavis Backend      │
│  (Tauri 2.0 + React) │                     │   (Control Plane)    │
└──────────┬───────────┘                     └──────────┬───────────┘                                    │               │LiveKit API
           │  WebRTC                                    ▼ (SFU mode)
           │  P2P ──────────────────────────────►┌─────────────
         │                                      │  LiveKit SFU │
           └──────────────────────────────────────►│  (media)     │
                                                  └──────────────┘

The backend is the **control plane only**: room lifecycle, channel membership, invite validation, capacity enforcement, JWT issuance, signaling relay, TURN credentials. It never touches media. In P2P mode,flows directly, media flowsthrough LiveKit.

The CLI test client (`clients/cli-test/`) speaks the same WebSocket API and is used for dev and integration testing.
                                                 ## Project Str

| Directory | Description |                      |-----------|-
| `wavis-backend/` | Control plane server (Axum, room state) |
| `clients/wavis-gui/` | Desktop app (Tauri 2.0 + React) |
| `clients/shared/` | Shared client library (WebRTC, audio pipeline, signaling) |                           | `clients/cli dev and
`doc/` | Quickstart, testing docs, deployment guides |

## Getting Started

**Prerequisites:** Rust toolchain, Node.js LTS, [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.

### Backend

```powershell                                    Copy-Item .env
cargo run -p wavis-backend

Desktop App

cd clients/wavis-gui
npm install
npx tauri dev
                                                 First run compa few minutes.

Loopback Test (no server needed)                 
cargo run -p wavis-cli-test -- --loopback

Runs mic → WebRTC → speakers locally to verify audio before connecting to a backend.

For full setup including Docker, LiveKit, TURN, amulti-terminalSTART.md.

Channels and Invites                             Rooms are scoptes a channel,generates an invite code, and shares it out-of-band. The backend validates the code, enforces capacity (max 6), and issues a JWT for media access. Codes support expimax-use limitsst peer leaves local development.

Identity

Wavis uses credential-free device registration — no passwords, no OAuth. Each device gets a recovery phrase (Argon2id, AES-256-GCM encrypted at rest). Additional devices can be paired via QR or a short code. Recovery on a new device requires only the recovery phrase. Refresh tokens use HMAC-SHA256 hashing with reuse detection and session epoch for atomic logout-all.             
Voice Modes

┌──────┬──────────────┬──────────────────────────│ Mode │ Parti             │
─────┴──────────────┴─────────────────────────────────┘

The backend auto-selects the mode at startup. The livekit feature flag on clients/shared gates the LiveKit Rust SDK dependency.

Security

- DTLS-SRTP transport encryption (WebRTC standard); no E2EE (in SFU mode, LiveKit can access media)
- Invite-only rooms; invite codes enforce expiratmax-use, and r
- Server-enforced capacity (max 6); host can kick participants                                     - Mute is adviedia-level- Per-IP and ptemporary IPbans for abuse
- Opaque error responses; no sensitive field leakage; no_sensitive_logs integration test enforces this - 22 securityiro/steering/security.md

Testing

cargo test --workspace

All automated tests use mocks — no running server, no audio hardware, no LiveKit needed.

cargo clippy --workspace -- -D warnings
                                                 For manual tescle, ratelimiting, kick, SDP size limits, room cleanup), see doc/testing/.
                                                 LiveKit E2E Te

These run the real LiveKitSfuBridge against a live LiveKit server. They verify room lifecycle, media token issuance, JWT validity, and room cleanup.

1. Start LiveKit and Redis:

docker compose up -d redis livekit
                                                 2. Run the tes

$env:LIVEKIT_API_KEY="devkey";                   $env:LIVEKIT_A$env:LIVEKIT_HOST="ws://localhost:7880"; $env:SFU_JWT_SECRET="dev-secret-32-bytes-minimum!!!XX"; cargo test -p wavis-backend --test livekit_e2e_integration -- --ignored --test-threads=1
                                                 Credentials arr-compose.yml.--test-threads=1 prevents room name collisions.
                                                 3. Tear down:

docker compose down redis livekit                
These tests also run in CI via the LiveKit E2E GitHub Actions workflow on PRs touching livekit_bridge.rs, sfu_relay.rs, livekit.yaml, or docker-compose.yml.

Docs                                             
- doc/QUICKSTART.md — commands and runbooks
- doc/TESTING.md — test strategy and manual test walkthroughs
- doc/turn_credentials_audit.md — TURN topology, investigation steps, rollback procedure

Contributing

Follow the layering rules:

- Handlers — transport only (WebSocket / HTTP)
- Domain — business logic (channels, invites, JWT, capacity, screen share)
- State — in-memory storage and concurrency

Don't duplicate WebRTC, signaling, or permission logic.
Shared types lteering/ forarea-specific architectural guidance before touching any subsystem.

Security-sensi, rate limiting, JWT, phrase handling) requires property tests. Don't log sensitive fields — the no_sensitive_logs integration test enforces this at CI time.

New SignalingMessage variants require updates in: mod.rs, validation.rs, proptest_support.rs, ws.rs, call_session.rs. New Tauri window labels must be added to capabilities/default.json.

Run cargo test --workspace and cargo clippy --workspace --
-D warnings be

Why Rust?

Memory safety without a GC, predictable latency, strong
async ecosystess backend andnative clients. No runtime overhead, no hidden allocations.

License

MIT — see LICENSE.
