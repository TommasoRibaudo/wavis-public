# wavis-client

Interactive CLI client for Wavis voice rooms.

## Build & Run

```bash
# With LiveKit audio
cargo run -p wavis-client --features livekit -- --server wss://example.cloudfront.net/ws
cargo run -p wavis-client --features livekit -- --server ws://localhost:3000/ws
# Proxy mode only (no LiveKit)
cargo run -p wavis-client -- --server wss://example.cloudfront.net/ws
```

## Commands

| Command | Description |
|---------|-------------|
| `create <room-id>` | Create a room (dev convenience, requires `REQUIRE_INVITE_CODE=false`) |
| `join <room-id> <invite-code>` | Join a room with an invite code |
| `invite [max-uses]` | Generate an invite code (host only) |
| `revoke <invite-code>` | Revoke an invite code (host only) |
| `leave` | Leave the current room |
| `status` | Show current room, peer ID, participants, SFU mode |
| `help` | Show available commands |
| `quit` | Leave room (if any) and exit |

## Typical Flow

```
> create my-room
OK: Joined room my-room as peer abc123 (1 peer(s)) [participants: none]

> invite
OK: Invite created: code=XK9F2M expires_in_secs=3600 max_uses=5

> status
OK: Room: my-room | Peer: abc123 | Participants: 1 | Mode: LiveKit | [none]

> leave
OK: Left room.

> quit
```

## Output Prefixes

- `OK:` — successful operations
- `ERR:` — errors
- `EVENT:` — async events (participant joined/left, LiveKit connected, disconnected)

## Distributing a Standalone Binary

You can share the client as a single executable — no Rust toolchain or dependencies needed on the target machine.

### Build a release binary

```bash
# With LiveKit/SFU support
cargo build --release -p wavis-client --features livekit

# P2P only (no LiveKit)
cargo build --release -p wavis-client
```

The binary will be at `target/release/wavis-client.exe` (Windows) or `target/release/wavis-client` (Linux/macOS).

In a terminal, go to where you have placed the app and run this:
```bash
./wavis-client --server wss://example.cloudfront.net/ws --show-secrets
```
### Cross-platform notes

- The binary targets whatever platform you build on. To target a different OS, either build natively on that machine or use [`cross`](https://github.com/cross-rs/cross):
  ```bash
  cargo install cross
  cross build --release --target x86_64-unknown-linux-gnu -p wavis-client --features livekit
  ```
- The recipient needs network access to the Wavis backend server. Pass the server URL at launch:
  ```bash
  ./wavis-client --server wss://your-server.example.com/ws
  ```
- Invite codes are redacted by default. To show them in full, pass `--show-secrets`:
  ```bash
  ./wavis-client --server wss://your-server.example.com/ws --show-secrets
  ```

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean exit (quit / Ctrl+C) |
| 1 | WebSocket connection failed on startup |
| 2 | WebSocket dropped while running |
