## Description

<!-- What does this PR do? Why? -->

## Type of Change

- [ ] Bug fix
- [ ] New feature
- [ ] Refactor
- [ ] Dependency update
- [ ] Documentation / config only

---

## Security Checklist

> Complete this section for any PR that touches signaling or token paths:
> `domain/jwt.rs`, `domain/livekit_bridge.rs`, `domain/relay.rs`,
> `domain/sfu_relay.rs`, `handlers/ws.rs`

If this PR does **not** touch any of those files, check this box and skip the rest:
- [ ] This PR does not touch signaling or token paths — checklist not applicable

Otherwise, confirm each item below:

### Sensitive Data Logging (Req 5.1–5.4)
- [ ] No JWT strings, raw tokens, or MediaToken values are passed to log macros
- [ ] No invite code values are passed to log macros (lengths/counts are OK)
- [ ] No SDP content is passed to log macros (lengths/types are OK)
- [ ] No ICE candidate strings are passed to log macros (counts/types are OK)
- [ ] `cargo test --test no_sensitive_logs` passes locally (also enforced by CI: `backend-ci.yml`)

### Message Size Limits (Req 3.5, 3.8)
- [ ] SDP payloads are validated against `MAX_SDP_BYTES` (64 KB) before forwarding
- [ ] ICE candidate payloads are validated against `MAX_ICE_CANDIDATE_BYTES` (2 KB) before forwarding
- [ ] Oversized payloads return a descriptive error and are not forwarded

### Server-Enforced Identity (Req 2.3, 2.4)
- [ ] All post-join message dispatch uses `session.participant_id` as sender identity
- [ ] No client-supplied identity fields are trusted for routing or authorization

### Domain-Enforced Action Authorization (Req 3.1, 3.2, 3.6)
- [ ] Action messages (kick, mute) are rejected in P2P rooms
- [ ] Host-only actions check `session.role == Host` in the handler (Tier 1)
- [ ] Domain functions (`handle_kick`, etc.) independently validate role (Tier 2 / defense in depth)
- [ ] Action messages are never forwarded through the relay path

### Token Scope and Lifetime (Req 1.1–1.7)
- [ ] MediaTokens include `iss` and `aud` claims
- [ ] Token TTL is ≤ 600s (default) — no unbounded lifetimes
- [ ] `validate_media_token` checks both `iss` and `aud`
- [ ] Revoked participants are blocked from token re-issuance within the TTL window

---

## Tests

- [ ] `cargo test -p wavis-backend` passes
- [ ] New property tests added for any new correctness properties
- [ ] No tests were deleted or disabled to make the build pass
