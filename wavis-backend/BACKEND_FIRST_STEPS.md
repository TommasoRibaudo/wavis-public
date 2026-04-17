# Wavis Backend: Steering-Aligned TODO (Rust Beginner)

Use this checklist as the implementation order for Phase 2 P2P signaling.

References:
- `.kiro/steering/signaling_protocol.md`
- `.kiro/steering/webrtc_p2p.md`
- `.kiro/steering/rust_webrtc_patterns.md`

## 1. Foundation (Current Service)

- [x] Add `GET /health` returning `{"ok": true}`
- [x] Add `/ws` route with `WebSocketUpgrade`
- [x] Use `Result`-based startup error handling (`main() -> io::Result<()>`)
- [x] Add connect/disconnect logging in `handle_socket`
- [x] Add debug endpoint `GET /debug/rooms` for runtime room visibility

## 2. Protocol Types (From signaling_protocol.md)

- [x] Centralize protocol types in `shared/src/signaling/mod.rs`
- [x] Define incoming messages: `join`, `offer`, `answer`, `ice_candidate`, `leave`
- [x] Define outgoing messages: `joined`, `error`, relayed `offer/answer/ice_candidate`
- [x] Implement `serde` serialize/deserialize with tagged `type` field
- [x] Add parser validation for malformed payloads

## 3. In-Memory Signaling State (From webrtc_p2p.md)

- [x] Create `src/state.rs`
- [x] Add room model (`room_id -> peers`)
- [x] Add peer model (`peer_id`, sender channel, joined room)
- [x] Enforce 1:1 room behavior for Phase 2
- [x] Validate room membership before relaying signaling messages

## 4. WebSocket Message Flow (From signaling_protocol.md exchange flow)

- [x] On socket connect, create peer session and ID
- [x] Handle `join`: register peer in room and send `joined`
- [x] Handle `offer`: relay to the other peer in same room
- [x] Handle `answer`: relay to the other peer in same room
- [x] Handle `ice_candidate`: relay immediately (trickle ICE)
- [x] Handle `leave` and disconnect: remove peer and cleanup room state
- [x] Return `error` message for invalid room, peer not found, malformed message

## 5. Module Cleanup (From rust_webrtc_patterns.md)

- [x] Keep `src/main.rs` limited to startup + route registration
- [x] Move ws handlers to `src/handlers/ws.rs`
- [x] Wire shared app state into router (`with_state(...)`)
- [x] Keep signaling message types centralized (no duplicate ad-hoc structs)

## 6. Quality Gates

- [ ] Run `cargo fmt` (last run not yet recorded)
- [x] Run `cargo clippy`
- [x] Add unit tests for protocol parsing/validation
- [x] Add unit tests for room + peer state transitions
- [x] Add integration test for two peers join + offer/answer relay
- [x] Add integration test for ICE candidate relay

## Immediate next task

- [ ] Run `cargo fmt` and commit formatted output
- [ ] Replace `println!`/`eprintln!` with structured `tracing` logs
