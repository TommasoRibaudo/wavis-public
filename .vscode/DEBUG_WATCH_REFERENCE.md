# Wavis — Debug Watch Reference

Quick reference for variables, expressions, and watchpoints to use in the
VS Code **Watch** panel and **Debug Console** (LLDB) when debugging each
launch configuration.

---

## Table of Contents

1. [Run Backend](#1-run-backend)
2. [Run CLI Client](#2-run-cli-client)
3. [Run CLI Test Client (loopback)](#3-run-cli-test-client-loopback)
4. [Run CLI Test Client (server)](#4-run-cli-test-client-server)
5. [Run CLI Test Client (SFU)](#5-run-cli-test-client-sfu)
6. [Unit Tests — Backend / Shared / Client-Shared](#6-unit-tests)
7. [Single Test — Backend / Client-Shared](#7-single-test)
8. [Integration Tests — Backend / Client-Shared](#8-integration-tests)
9. [Stress Harness (all variants)](#9-stress-harness)
10. [GUI Surface Test](#10-gui-surface-test)
11. [Test Entire Workspace](#11-test-entire-workspace)
12. [LLDB Tips & Cheat Sheet](#12-lldb-tips--cheat-sheet)

---

## 1. Run Backend

> `wavis-backend` with Postgres + LiveKit, `test-metrics` feature.

### Watch Variables

#### AppState (`app_state.rs`)
```
app_state.room_state              -- Arc<InMemoryRoomState>
app_state.connections             -- Arc<LiveConnections>
app_state.sfu_health_status       -- Arc<RwLock<SfuHealth>>
app_state.sfu_url                 -- String
app_state.invite_store            -- Arc<InviteStore>
app_state.join_rate_limiter       -- Arc<JoinRateLimiter>
app_state.abuse_metrics           -- Arc<AbuseMetrics>
app_state.temp_ban_list           -- Arc<TempBanList>
app_state.ip_connection_tracker   -- Arc<IpConnectionTracker>
app_state.ws_rate_limit_config    -- WsRateLimitConfig
app_state.global_ws_limiter       -- Arc<GlobalRateLimiter>
app_state.global_join_limiter     -- Arc<GlobalRateLimiter>
app_state.require_invite_code     -- bool
app_state.require_tls             -- bool
app_state.jwt_secret              -- Arc<Vec<u8>>
app_state.auth_jwt_secret         -- Arc<Vec<u8>>
app_state.auth_rate_limiter       -- Arc<AuthRateLimiter>
app_state.channel_rate_limiter    -- Arc<ChannelRateLimiter>
app_state.db_pool                 -- sqlx::PgPool
app_state.active_room_map         -- Arc<RwLock<HashMap<Uuid, String>>>
app_state.refresh_token_ttl_days  -- u32
```

#### Room State (`state.rs`)
```
room_state.rooms                  -- RwLock<HashMap<RoomId, Arc<RwLock<RoomMembers>>>>
room_state.peer_to_room           -- RwLock<HashMap<PeerId, RoomId>>

# Inside a RoomMembers / RoomInfo:
room_members.peers                -- Vec<PeerId>
room_info.room_type               -- RoomType (P2P | Sfu)
room_info.max_participants        -- u8
room_info.participants            -- Vec<ParticipantInfo>
room_info.active_shares           -- HashSet<String>
room_info.share_permission        -- SharePermission (AllParticipants | HostOnly)
room_info.media_connected         -- HashSet<String>
room_info.revoked_participants    -- HashMap<String, Instant>
room_info.created_at              -- Instant
```

#### Connections (`connections.rs`)
```
connections.senders               -- RwLock<HashMap<String, mpsc::UnboundedSender<String>>>
```

#### WebSocket Rate Limiting (`handlers/ws.rs`)
```
ws_rate_limit_config.window       -- Duration
ws_rate_limit_config.max_messages -- u32
ws_rate_limit_config.burst_max    -- u32
ws_rate_limit_config.burst_window -- Duration
ws_rate_limit_config.action_max   -- u32
ws_rate_limit_config.action_window -- Duration
ws_rate_limit_config.max_json_depth -- u32

# Per-connection rate limiter (local to ws handler):
rate_limiter.message_count        -- u32
rate_limiter.burst_count          -- u32
rate_limiter.action_count         -- u32
rate_limiter.window_start         -- Instant
rate_limiter.burst_window_start   -- Instant
rate_limiter.action_window_start  -- Instant
```

#### Abuse Metrics (`domain/abuse_metrics.rs`)
```
abuse_metrics.ws_rate_limit_rejections           -- AtomicU64
abuse_metrics.ws_burst_rejections                -- AtomicU64
abuse_metrics.action_rate_limit_rejections       -- AtomicU64
abuse_metrics.join_rate_limit_rejections          -- AtomicU64
abuse_metrics.join_invite_rejections              -- AtomicU64
abuse_metrics.connections_closed_rate_limit       -- AtomicU64
abuse_metrics.payload_size_violations             -- AtomicU64
abuse_metrics.connections_rejected_ip_cap         -- AtomicU64
abuse_metrics.connections_rejected_temp_ban       -- AtomicU64
abuse_metrics.global_ws_ceiling_rejections        -- AtomicU64
abuse_metrics.global_join_ceiling_rejections      -- AtomicU64
abuse_metrics.schema_validation_rejections        -- AtomicU64
abuse_metrics.state_machine_rejections            -- AtomicU64
abuse_metrics.screen_share_rejections             -- AtomicU64
abuse_metrics.revoke_authorization_rejections     -- AtomicU64
abuse_metrics.tls_proto_rejections                -- AtomicU64
abuse_metrics.invite_usage_anomalies              -- AtomicU64
```

#### Temp Ban List (`domain/temp_ban.rs`)
```
temp_ban_list.bans                -- RwLock<HashMap<IpAddr, BanEntry>>
temp_ban_list.violations          -- RwLock<HashMap<IpAddr, ViolationWindow>>
temp_ban_list.config.threshold    -- u32
temp_ban_list.config.ban_duration -- Duration
temp_ban_list.config.max_entries  -- usize
```

#### IP Tracker (`domain/ip_tracker.rs`)
```
ip_connection_tracker.connections -- RwLock<HashMap<IpAddr, u32>>
ip_connection_tracker.max_per_ip  -- AtomicU32
```

#### Invite Store (`domain/invite.rs`)
```
invite_store.invites              -- RwLock<HashMap<String, InviteRecord>>
invite_store.room_invite_counts   -- RwLock<HashMap<String, usize>>
invite_store.config.default_ttl   -- Duration
invite_store.config.default_max_uses    -- u32
invite_store.config.max_invites_per_room -- usize
invite_store.config.max_invites_global   -- usize

# Individual InviteRecord:
record.code                       -- String
record.room_id                    -- String
record.issuer_id                  -- String
record.remaining_uses             -- u32
record.revoked                    -- bool
record.expires_at                 -- Instant
```

#### Join Rate Limiter (`domain/join_rate_limiter.rs`)
```
join_rate_limiter.ip_total        -- RwLock<HashMap<IpAddr, SlidingWindow>>
join_rate_limiter.ip_failed       -- RwLock<HashMap<IpAddr, SlidingWindow>>
join_rate_limiter.code_attempts   -- RwLock<HashMap<String, SlidingWindow>>
join_rate_limiter.room_attempts   -- RwLock<HashMap<String, SlidingWindow>>
join_rate_limiter.connection_attempts -- RwLock<HashMap<String, SlidingWindow>>
join_rate_limiter.config          -- RwLock<JoinRateLimiterConfig>
```

#### Global Rate Limiter (`domain/global_rate_limiter.rs`)
```
global_ws_limiter.state           -- AtomicU64
global_ws_limiter.max_per_sec     -- u32
global_join_limiter.state         -- AtomicU64
global_join_limiter.max_per_sec   -- u32
```

#### Auth Rate Limiter (`domain/auth_rate_limiter.rs`)
```
auth_rate_limiter.config.register_max_per_ip  -- u32
auth_rate_limiter.config.register_window_secs -- u64
auth_rate_limiter.config.refresh_max_per_ip   -- u32
auth_rate_limiter.config.refresh_window_secs  -- u64
auth_rate_limiter.register_windows -- Mutex<HashMap<IpAddr, SlidingWindow>>
auth_rate_limiter.refresh_windows  -- Mutex<HashMap<IpAddr, SlidingWindow>>
```

#### Channel Rate Limiter (`domain/channel_rate_limiter.rs`)
```
channel_rate_limiter.config.max_per_user   -- u32
channel_rate_limiter.config.window_secs    -- u64
channel_rate_limiter.windows               -- Mutex<HashMap<Uuid, SlidingWindow>>
```

#### Chat Rate Limiter (`domain/chat_rate_limiter.rs`)
```
chat_rate_limiter.tokens          -- f64
chat_rate_limiter.max_tokens      -- f64
chat_rate_limiter.refill_rate     -- f64
chat_rate_limiter.last_check      -- Instant
```

### Recommended Breakpoints

| File | What to break on |
|------|------------------|
| `handlers/ws.rs` | WS upgrade entry, rate limit checks |
| `domain/relay.rs` | `relay_message()` entry |
| `domain/sfu_relay.rs` | `handle_sfu_*` functions |
| `domain/voice_orchestrator.rs` | room create, participant add |
| `domain/screen_share.rs` | `start_share`, `stop_share` |
| `domain/auth.rs` | device registration, token refresh |
| `domain/jwt.rs` | token sign, token validate |
| `handlers/auth_routes.rs` | each route handler |
| `handlers/channel_routes.rs` | channel CRUD handlers |
| `state.rs` | room add/remove participant |
| `connections.rs` | `insert`, `remove` |
| `main.rs` | background task spawns |

### Useful LLDB Expressions
```lldb
# Count active connections
p connections.senders.read().unwrap().len()

# Check if an IP is banned
p temp_ban_list.bans.read().unwrap().contains_key(&ip)

# Snapshot abuse metrics
p abuse_metrics.ws_rate_limit_rejections.load(Ordering::Relaxed)
p abuse_metrics.join_rate_limit_rejections.load(Ordering::Relaxed)

# Count rooms
p room_state.rooms.read().unwrap().len()
```

---

## 2. Run CLI Client

> Full REPL client connecting to `--server <wsUrl>`.

### Watch Variables

#### Call Manager (`webrtc.rs`)
```
call_manager.state                -- Arc<Mutex<CallState>>
  # CallState: Idle | Negotiating | Connecting | Connected | Failed | Closed
call_manager.ice_config           -- Arc<Mutex<IceConfig>>
call_manager.audio                -- Arc<A> (AudioBackend)
call_manager.pc_backend           -- Arc<P> (PeerConnectionBackend)
```

#### Call Session (`call_session.rs`)
```
call_session.call_manager         -- Arc<CallManager<A, P>>
call_session.signaling            -- Arc<SignalingClient<W>>
```

#### Signaling Client (`signaling.rs`)
```
signaling_client.ws               -- W (WebSocketConnection impl)
signaling_client.handler          -- MessageHandler
```

#### ICE Config (`ice_config.rs`)
```
ice_config.stun_urls              -- Vec<String>
ice_config.turn_urls              -- Vec<String>
ice_config.turn_username          -- String
ice_config.turn_credential        -- String
```

### Recommended Breakpoints

| File | What to break on |
|------|------------------|
| `wavis-client/src/main.rs` | WS connect |
| `wavis-client/src/commands.rs` | each command handler |
| `clients/shared/src/signaling.rs` | `send_text`, message receive |
| `clients/shared/src/webrtc.rs` | `CallState` transitions |
| `clients/shared/src/call_session.rs` | session start/end |

### Useful LLDB Expressions
```lldb
# Current call state
p *call_manager.state.lock().unwrap()

# ICE servers configured
p ice_config.stun_urls
p ice_config.turn_urls
```

---

## 3. Run CLI Test Client (loopback)

> Local WebRTC loopback — no signaling server, audio pipeline only.

### Watch Variables

#### Audio Pipeline (`audio_pipeline_real.rs`)
```
encoder.current_bitrate           -- u32
encoder.encoder                   -- opus::Encoder
decoder.decoder                   -- opus::Decoder
audio_processor.mode              -- ApmMode
```

#### Audio Meter (`audio_meter.rs`)
```
meter.label                       -- &'static str
meter.sum_sq                      -- AtomicU64
meter.sample_count                -- AtomicU64
meter.peak                        -- AtomicU64
meter.clipped                     -- AtomicU64
meter.frame_count                 -- AtomicU64
```

#### WebRTC / Connection State (`webrtc.rs`)
```
connection_state                  -- ConnectionState
  # New | Checking | Connected | Completed | Failed | Disconnected | Closed
call_state                        -- CallState
  # Idle | Negotiating | Connecting | Connected | Failed | Closed
```

### Recommended Breakpoints

| File | What to break on |
|------|------------------|
| `cli-test/src/main.rs` | loopback mode entry |
| `webrtc_backend.rs` | create offer, set answer |
| `audio_pipeline_real.rs` | pipeline start/stop |
| `cpal_audio.rs` | device open, audio callback |
| `audio_mixer.rs` | mix function |
| `sdp_ice_guards.rs` | validation failures |

### Useful LLDB Expressions
```lldb
# Audio meter snapshot
p meter.peak.load(Ordering::Relaxed)
p meter.frame_count.load(Ordering::Relaxed)

# Encoder bitrate
p encoder.current_bitrate
```

---

## 4. Run CLI Test Client (server)

> P2P mode against a running backend with room + invite.

### Watch Variables

All from [CLI Client](#2-run-cli-client) plus:

#### TungsteniteWs (`cli-test/src/lib.rs`)
```
ws                                -- TungsteniteWs wrapper
```

#### Signaling Messages (`shared/src/signaling/mod.rs`)
```
msg                               -- SignalingMessage enum
  # Variants: Join, Joined, Offer, Answer, IceCandidate, PeerLeft,
  #   Leave, Error, JoinRejected, InviteCreate, InviteCreated,
  #   InviteRevoke, InviteRevoked, ParticipantJoined, ParticipantLeft,
  #   RoomState, MediaToken, KickParticipant, MuteParticipant,
  #   StartShare, ShareStarted, StopShare, ShareStopped, StopAllShares,
  #   ShareState, SetSharePermission, SharePermissionChanged,
  #   CreateRoom, RoomCreated, Auth, AuthSuccess, AuthFailed,
  #   JoinVoice, ChatSend, ChatMessage, Ping
```

### Recommended Breakpoints

| File | What to break on |
|------|------------------|
| `cli-test/src/lib.rs` | `wait_for_room_created`, `wait_for_joined_result` |
| `shared/signaling/mod.rs` | message deserialization |
| `webrtc_backend.rs` | ICE candidate add |
| `webrtc.rs` | connection state change |

### Useful LLDB Expressions
```lldb
# Inspect signaling message variant
p msg

# Check room/invite args
p room_id
p invite_code
```

---

## 5. Run CLI Test Client (SFU)

> SFU mode with LiveKit for multi-party calls.

### Watch Variables

All from [CLI Test Client (server)](#4-run-cli-test-client-server) plus:

#### LiveKit / Room Session (`room_session.rs`)
```
# MockLiveKitConnection (in tests):
mock_lk.calls                     -- Arc<Mutex<Vec<MockLiveKitCall>>>
mock_lk.connect_result            -- Arc<Mutex<Result<(), String>>>
mock_lk.available                 -- Arc<Mutex<bool>>

# RoomError variants:
  # NotInRoom | AlreadyInRoom | SfuConnectionFailed(String)
  # PublishFailed(String) | Audio(String) | Signaling(String)
```

### Additional Breakpoints

| File | What to break on |
|------|------------------|
| `livekit_connection.rs` | room connect, disconnect |
| `livekit_connection.rs` | track publish, subscribe |
| `room_session.rs` | participant add/remove |

### Useful LLDB Expressions
```lldb
# Check if LiveKit is available
p lk_connection.is_available()

# MediaToken from signaling
p media_token
```

---

## 6. Unit Tests

> `Test Backend (all)`, `Test Shared Signaling (all)`, `Test Client Shared (all)`

### Watch Variables

Same as the corresponding binary config, but focus on:

```
# Test assertions — break on these locals:
expected                          -- the expected value
actual                            -- the actual/computed value
result                            -- Result<T, E> from function under test

# Mock objects:
mock_sfu_bridge                   -- MockSfuBridge (backend tests)
mock_audio                        -- AudioPipelineMock (client tests)
```

### Recommended Breakpoints

- Inside the failing test function body
- `#[should_panic]` tests — break at the panic point
- Any `assert!` / `assert_eq!` macro expansion sites
- Mock trait `impl` methods

### Call Stack Filter
```
# Hide test harness noise:
!std::panicking::*
!test::*
!core::ops::*
```

---

## 7. Single Test

> `Test Backend (single)`, `Test Client Shared (single)`
>
> These have `stopOnEntry: true` so you can set breakpoints before the test runs.

### Watch Variables

Same as [Unit Tests](#6-unit-tests), scoped to one test.

### Tips
- Use exact test path for the `testName` input:
  `domain::relay::tests::test_relay_offer`
- Step through with F10/F11 from the entry stop

---

## 8. Integration Tests

> All integration test configs (Backend all, single file, single test, Client-Shared all).

### Watch Variables

```
# Test infrastructure (common across integration tests):
test_server                       -- in-process backend instance
ws_client                         -- WebSocket test client
response                          -- HTTP response (status + body)
db_pool                           -- PgPool test database
invite_code                       -- generated/consumed invite

# Plus all backend watch variables from section 1
```

### Breakpoints by Test Category

| Category | Test Files | Break On |
|----------|-----------|----------|
| **Auth** | `auth_integration`, `ws_auth_integration`, `jwt_validation` | token gen/validate/refresh |
| **Signaling** | `signaling_relay_integration`, `p2p_relay_integration`, `sfu_relay_integration` | message relay, join/leave |
| **Security** | `abuse_controls_integration`, `security_hardening_integration` | rate limit triggers, ban enforcement |
| **Channels** | `channel_integration`, `channel_rest_integration` | CRUD, membership, invites |
| **Voice** | `voice_orchestration_integration`, `voice_ws_integration` | room create, participant flow |
| **Screen Share** | `screen_share_integration`, `screen_share_ws_integration` | share lifecycle, permissions |
| **Concurrency** | `concurrency`, `reconnect_drop_matrix` | race conditions, reconnection |
| **Cleanup** | `cleanup_integration` | sweep tasks, expiry |

---

## 9. Stress Harness

> All 4 variants: CI scale, local scale, single scenario, external backend.

### Watch Variables

#### Config (`tools/stress/src/config.rs`)
```
scale.concurrent_clients          -- usize
scale.actions_per_client          -- usize
scale.total_actions               -- usize
scale.rss_growth_threshold_pct    -- f64
scale.cpu_spike_threshold_x       -- f64
scale.repetitions                 -- usize

thresholds.join_p95               -- Duration
thresholds.join_p99               -- Duration
thresholds.message_p95            -- Duration
thresholds.message_p99            -- Duration
thresholds.flood_healthy_p95      -- Duration
```

#### Test Context (`tools/stress/src/config.rs`)
```
ctx.ws_url                        -- String
ctx.metrics_url                   -- String
ctx.scale                         -- ScaleConfig
ctx.app_state                     -- Option<AppState>   (None for external)
ctx.capabilities                  -- Vec<Capability>
ctx.rng_seed                      -- u64
```

#### Results (`tools/stress/src/results.rs`)
```
result.name                       -- String (scenario name)
result.passed                     -- bool
result.duration                   -- Duration
result.actions_per_second         -- f64
result.p95_latency                -- Duration
result.p99_latency                -- Duration
result.violations                 -- Vec<InvariantViolation>

violation.invariant               -- String
violation.expected                -- String
violation.actual                  -- String
```

#### Process Stats (`tools/stress/src/process_stats.rs`)
```
stats.pid                         -- Option<u32>
stats.baseline_rss_kb             -- Option<u64>
stats.peak_rss_kb                 -- Option<u64>
```

#### Latency Tracker (`tools/stress/src/results.rs`)
```
tracker.samples                   -- Vec<Duration>
```

### Recommended Breakpoints

| File | What to break on |
|------|------------------|
| `runner.rs` | scenario start/end |
| `runner.rs` | invariant check failures |
| `assertions.rs` | `check_violation()` |
| `client.rs` | WS connect/send/recv |
| `process_stats.rs` | RSS/CPU sample |
| `results.rs` | result aggregation |
| `scenarios/<name>.rs` | scenario `run()` entry |

### Variant-Specific Notes

| Variant | `RUST_LOG` | Notes |
|---------|-----------|-------|
| **CI scale** (100 clients) | `info,stress_harness=debug` | Use log points, not stop-breakpoints |
| **Local scale** (1000 clients) | `warn,stress_harness=info` | LLDB may struggle; conditional breaks only |
| **Single scenario** | `debug,stress_harness=trace` | Best for deep-dive; stop-breakpoints OK |
| **External backend** | `info,stress_harness=debug` | No backend internals; focus on client timing |

### Useful LLDB Expressions
```lldb
# Check scenario result
p result.passed
p result.p95_latency
p result.violations.len()

# RSS growth
p stats.peak_rss_kb.unwrap() - stats.baseline_rss_kb.unwrap()

# Latency percentile (after tracker populated)
p tracker.samples.len()
```

### All 27 Stress Scenarios (for `stressScenario` input)
```
join_flood              brute_force_invite       join_leave_storm
invite_exhaustion_race  invite_revocation_race   invite_expiry_race
cross_room_invite       authz_fuzz               replay_attack
token_confusion         screen_share_race        stop_all_shares_authz
host_directed_stop      share_state_consistency  multi_share_flood
message_flood           oversized_payload        profile_color_fuzz
turn_credential_abuse   slowloris                idle_connection_flood
log_leak                auth_brute_force         refresh_token_reuse
auth_state_machine_race cross_secret_token_confusion
chat_flood              chat_authz_fuzz
```

---

## 10. GUI Surface Test

> 17 REST API scenarios testing GUI-facing endpoints.

### Watch Variables

#### Test Context (`tools/gui-surface-test/src/config.rs`)
```
ctx.base_url                      -- String
ctx.metrics_token                 -- String
ctx.http_client                   -- reqwest::Client
```

#### Per-Scenario
```
response                          -- reqwest::Response
status                            -- StatusCode
body                              -- serde_json::Value (or typed DTO)
auth_token                        -- String (device JWT)
channel_id                        -- Uuid
```

### Recommended Breakpoints

| File | What to break on |
|------|------------------|
| `runner.rs` | scenario start/end |
| `client.rs` | HTTP request send |
| `client.rs` | response parsing |
| `scenarios/*.rs` | assertion points |
| `helpers.rs` | setup/teardown |

### All 17 GUI Surface Scenarios
```
auth_flow                channel_lifecycle        channel_detail_read
channel_detail_mutations channel_detail_role_matrix channel_detail_concurrency
voice_status             voice_room_participants  voice_room_host
voice_room_share         voice_room_reconnect     error_edges
media_token              mute_sync                screen_share_lifecycle
volume_control           media_reconnect
```

---

## 11. Test Entire Workspace

> All tests across all 8 crates.

### Watch Variables
Combination of all crate-specific variables. Focus on cross-crate boundaries:
```
# Signaling types flowing through backend and client:
msg: SignalingMessage              -- shared crate type used everywhere
```

### Notes
- `RUST_LOG=info` — trace is too verbose for full workspace
- Not ideal for debugging; prefer single-crate/single-test configs
- Best used for CI validation and pre-merge checks

---

## 12. LLDB Tips & Cheat Sheet

### Debug Console Commands

```lldb
# Print a variable with full type info
p variable_name
fr v variable_name

# Print all local variables
fr v -a

# Print specific struct field
p app_state.require_invite_code

# Dereference Arc/Box/Rc (Rust pretty-printers handle this)
p *arc_variable

# Read AtomicU64 value
p atomic_var.load(std::sync::atomic::Ordering::Relaxed)

# Read RwLock contents (only when lock is not held)
p *rwlock_var.read().unwrap()

# Read Mutex contents
p *mutex_var.lock().unwrap()

# HashMap length
p hash_map.len()

# Vec length
p vec_var.len()

# Check Option variant
p option_var.is_some()
p option_var.is_none()

# Format as hex/binary
p/x numeric_var
p/t numeric_var

# Set a watchpoint (break when value changes)
w set var variable_name

# Conditional breakpoint
br set -f file.rs -l 42 -c 'room_id == "test-room"'

# Log point (print without stopping)
br set -f file.rs -l 42 -o true -C 'p variable_name'

# List all breakpoints
br list

# Continue to next breakpoint
c

# Step over / step into / step out
n / s / finish

# Print backtrace
bt

# Print backtrace filtering to project frames only
bt -f wavis
```

### VS Code Watch Panel Expressions

Add these to the Watch panel (`Ctrl+Shift+D` > Watch section):

#### Backend — Always Useful
```
app_state.connections
app_state.room_state
app_state.abuse_metrics
app_state.temp_ban_list
app_state.invite_store
app_state.ws_rate_limit_config
```

#### Client — Always Useful
```
call_manager.state
ice_config
signaling_client.ws
```

#### Stress — Always Useful
```
ctx.scale
result.passed
result.violations
stats.peak_rss_kb
```

### Conditional Breakpoint Examples

```
# Break only when a specific room is joined
room_id == "test-room"

# Break only on error signaling messages
matches!(msg, SignalingMessage::Error(_))

# Break when rate limit is exceeded
rate_limiter.message_count >= rate_limiter.config_max

# Break when an IP gets banned
ban_entry.is_some()

# Break when connections exceed threshold
connections.senders.read().unwrap().len() > 50

# Break on specific signaling variant
msg.is_join_rejected()
```

### Environment Variable Quick Reference

| Variable | Default | Used By |
|----------|---------|---------|
| `RUST_LOG` | (none) | All configs |
| `RUST_BACKTRACE` | `0` | All configs |
| `DATABASE_URL` | `.env` | Backend, integration tests |
| `SFU_JWT_SECRET` | `.env` | Backend |
| `AUTH_JWT_SECRET` | `.env` | Backend |
| `LIVEKIT_HOST` | `ws://localhost:7880` | Backend, SFU client |
| `LIVEKIT_API_KEY` | `.env` | Backend |
| `LIVEKIT_API_SECRET` | `.env` | Backend |
| `TEST_METRICS_TOKEN` | hardcoded | Stress harness |
| `WAVIS_STUN_URLS` | (none) | CLI clients |
| `WAVIS_TURN_URLS` | (none) | CLI clients |
| `PORT` | `3000` | Backend |
