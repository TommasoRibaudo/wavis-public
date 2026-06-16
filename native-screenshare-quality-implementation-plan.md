# Native Screenshare Quality Implementation Plan

## Goal

Improve the native custom screen-share quality path, smoothness, and diagnostics without a large architecture rewrite.

This patch should keep the current JPEG/base64 frame pipeline and leave `dynacast` / `adaptiveStream` enabled. The focus is to fix quality state propagation, align Rust and JS preset semantics, improve native LiveKit publish options, make diagnostics distinguish active sharing from missing sender stats, and recover native sender-side frame cadence.

Reference investigation: [native-screenshare-quality-findings.md](native-screenshare-quality-findings.md).

## Progress Tracker

- [x] Persist selected screen-share quality before start.
- [x] Prevent manual `max` from being downgraded by post-start profile initialization.
- [x] Sync selected quality into Rust before native capture starts.
- [x] Align Rust capture presets with the canonical policy.
- [x] Improve native generated-track publish options to reuse the screen-share policy builder.
- [x] Fix diagnostics so active sharing is distinct from missing sender stats.
- [ ] Run manual verification commands.

### Smoothness Recovery

- [x] Stop keepalive from re-decoding duplicate JPEG/base64 frames.
- [x] Add interval/rate bridge cadence diagnostics.
- [x] Add Rust capture cadence diagnostics.
- [x] Add Windows fallback/retry path for low WGC new-frame cadence.
- [ ] Defer binary transport rewrite unless real-frame decode remains too slow after the above.

## Expected Outcome

Selecting `max` before starting native screen share should result in:

- persisted JS selected quality = `max`
- `LiveKitModule.currentQuality = max`
- Rust receives `media_set_screen_share_quality(max)` before native capture starts
- Rust starts capture with the `max` preset
- post-start profile initialization does not downgrade `max` to `high`

Diagnostics should clearly distinguish:

- not sharing
- sharing, waiting for sender stats
- sharing with actual bitrate/FPS/resolution stats

## Current Smoothness Snapshot

The latest diagnostic snapshot shows quality and subscription stability are fixed, but sender-side cadence is still the primary risk:

- Source: 2560x1072 screen share.
- Target: 30 FPS from JS and 30 FPS from Rust capture config.
- Bridge polling: 3625 poll hits but only 294 new Rust sequence frames.
- Duplicates: 3331 duplicate sequence skips.
- Keepalive: 1923 keepalive writes.
- JS decode: average around 100 ms per real Rust frame in the stale snapshot. Keepalive writes now reuse the cached decoded frame instead of re-decoding duplicate JPEG/base64 payloads.
- WebRTC/network: no packet loss, `qualityLimit` is `none`, and the track is live.
- Runtime state: no SFU quality limit and no interruption state.

Interpretation: the remaining smoothness problem is sender-side native capture/bridge cadence, not network congestion or SFU subscription demand. The primary success metric is real Rust new-sequence FPS at the JS bridge, not keepalive writes or duplicate poll hits.

Ranked likely causes:

1. Windows capture backend cadence is too low: diagnostics showed thousands of poll hits but only a few hundred new Rust sequence frames. Fix first with interval cadence metrics and WGC-to-GDI retry.
2. Rust hot path is too expensive per frame: WGC readback, BGRA/RGBA conversion, downscale, RGB conversion, JPEG encode, and base64 encode must fit under the target frame budget.
3. JS bridge backpressure can still limit throughput: `createImageBitmap`, `VideoFrame`, and writer work can stall polling while frame work is in flight.
4. Keepalive cadence is for publication health only. It cannot create real motion without new Rust sequences.
5. Publish/subscription policy is secondary while packet loss is absent and `qualityLimit` is `none`.
6. Dynacast/adaptiveStream remains the lowest-priority A/B after sender cadence is healthy.

## Canonical Preset Policy

| Profile | Intended use | Capture/publish target |
| --- | --- | --- |
| `low` / `motion` | smooth motion | 1920x1080 @ 60fps, JPEG 85 |
| `high` / `detail` | text/detail | 2560x1440 @ 30fps, JPEG 92 |
| `max` | detail plus motion | 2560x1440 @ 60fps, JPEG 95 |

## Implementation Tasks

### 1. Persist Selected Screen-Share Quality Before Start

- Add or confirm a voice-room module-level selected screen-share quality state.
- Default it to `high`.
- Ensure `setShareQuality()` updates this persisted selected quality before delegating to the media module.
- Update `LiveKitModule.setScreenShareQuality()` so it stores `currentQuality = quality` even when no room or screen-share publication exists.
- Keep existing active-publication behavior: when a publication exists, still apply constraints and update share quality info.
- Only roll back `currentQuality` on an actual active-track constraint failure.

### 2. Prevent Manual `max` From Being Downgraded After Start

- Audit the post-start profile switch path that currently applies `detail`, which maps to `high`.
- Ensure selecting `max` before custom native share start remains `max` after startup.
- Recommended approach: if persisted selected quality is `max`, initialize profile-switch state without calling `setScreenShareQuality('high')`.
- Keep motion/detail auto-switch behavior for non-`max` selections unless tests show it conflicts with the selected-quality contract.

### 3. Sync Selected Quality Into Rust Before Capture

- Add a small helper in `voice-room.ts`, such as `syncNativeScreenShareQualityBeforeCapture()`.
- The helper should call:

  ```ts
  await invoke('media_set_screen_share_quality', { quality: selectedShareQuality });
  ```

- Call this helper immediately before every native `screen_share_start_source` and `screen_share_start` capture path.
- Include Windows WGC/GDI attempts and Linux portal/source paths where native capture is used.
- Do not silently ignore real failures. Log quality, platform/path, and error details. Continue only when the platform command is intentionally a no-op/stub.

### 4. Align Rust Capture Presets

- Update `ScreenShareConfig` preset mapping:
  - `low`: 1920x1080 @ 60fps, JPEG 85
  - `high`: 2560x1440 @ 30fps, JPEG 92
  - `max`: 2560x1440 @ 60fps, JPEG 95
- Update comments and tests in the Rust screen-capture config area.
- Keep invalid preset behavior unchanged unless current behavior is clearly broken.

### 5. Improve Native Publish Option Parity

- Audit browser screen-share publish options and native generated-track publish options.
- In `startNativeCapture()`, prefer building options from the same screen-share policy builder used by browser capture.
- Preserve native-specific fields:
  - `name: 'native-screen-share'`
  - `source: Track.Source.ScreenShare`
  - `stream: Track.Source.ScreenShare`
- Include screen-share-specific fields where supported:
  - `screenShareEncoding`
  - codec policy
  - simulcast/layer config
  - `degradationPreference`
- If LiveKit SDK generated tracks require different option names, keep the path type-safe but semantically equivalent.
- After native publish succeeds, initialize the same runtime behavior as browser screen share where applicable:
  - adaptive state
  - post-publish tuning
  - sender stats polling

### 6. Fix Diagnostics “Not Sharing” Status

- Extend the `diagnostics:voice-stats` payload emitted from `App.tsx`.
- Add explicit share activity fields:
  - `isSharing`
  - `shareMode`
  - `shareSourceName`
- Derive `isSharing` from actual voice-room/share state, not from `shareStats`.
- Treat these as sharing:
  - local custom video share is active
  - browser/fallback self share is active
  - local video share publication is active, if exposed
- Extend `DiagnosticsSnapshot` with:
  - `shareActive`
  - `shareMode`
  - `shareSourceName`
- Update diagnostics lifecycle timestamps to use `shareActive`, not `shareStats !== null`.
- In diagnostics UI/export:
  - if `shareActive === true && share === null`, show `Sharing, waiting for stats`
  - keep bitrate/FPS/resolution rows gated on real `share` stats
  - do not fabricate zero-valued stats

### 7. Reuse Decoded Frames For Keepalive Writes

- In `startNativeCapture()`, cache the last decoded drawable source from a real Rust frame.
- Treat a "real Rust frame" as a new Rust sequence, not a duplicate poll result or keepalive repeat.
- Keepalive writes must repaint/create output frames from the cached decoded source without re-running base64/JPEG decode.
- Keep keepalive counters separate from real captured/decoded frame counters.
- On a new real sequence:
  - decode the new JPEG/base64 payload once
  - replace the cached decoded source
  - release/close the previous cached decoded resource when the runtime object requires explicit cleanup
- Release cached decoded resources on source replacement and `stopNativeCapture()`.
- Ensure source replacement stops any active keepalive loop before publishing the replacement source.

### 8. Add Native Bridge Cadence Diagnostics

- Extend native bridge diagnostics with cumulative and interval cadence fields:
  - `rustNewSeqFps`
  - duplicate poll ratio
  - keepalive FPS
  - latest sequence age
  - real decode average
  - keepalive write average
- Keep existing screen-share diagnostics and WebRTC sender stats shape intact.
- Add new fields under the native bridge diagnostics object instead of replacing current stats.
- Export diagnostics when sender stats are present and when sender stats are still absent.
- Add warnings for:
  - `rustNewSeqFps < 5` while target FPS is 30 or higher
  - real decode average above one frame budget for the active target FPS

### 9. Add Rust Capture Cadence Diagnostics

- Count raw frame arrivals from the platform backend.
- Count emitted bridge frames.
- Count throttle drops.
- Track encode timing.
- Include interval rates as well as cumulative counts so diagnostics can separate startup behavior from sustained capture behavior.
- Add an explicit Windows capture backend or retry reason field if one does not already exist, using values equivalent to `wgc` and `gdi_poll`.

### 10. Add Windows Smoothness Fallback

- Detect very low WGC new-sequence cadence during startup and sustained capture.
- When WGC remains below the smoothness threshold, restart the same source with the existing GDI polling backend where supported.
- Preserve the LiveKit generated track and publish policy across the retry path.
- Preserve native publish options, selected quality, share source identity, diagnostics state, and sender stats polling.
- Record the backend transition and retry reason in diagnostics.
- Do not change browser fallback behavior or force Rust-side LiveKit publishing as part of this recovery path.

## API / Type Additions

- Extend native bridge diagnostics with cadence fields for cumulative counts and interval rates.
- Add a Windows capture backend/retry reason field if one does not already exist, with values equivalent to `wgc` and `gdi_poll`.
- Preserve existing screen-share diagnostics and WebRTC sender stats shape.
- Add fields under the native bridge diagnostics object rather than replacing current stats.

## Test Tasks

### GUI / TypeScript Tests

- Selecting `max` before starting a custom native share persists into `LiveKitModule.currentQuality`.
- Windows native custom share invokes `media_set_screen_share_quality` before `screen_share_start_source`.
- Post-start profile initialization does not downgrade persisted manual `max` to `high`.
- Native `publishTrack()` receives screen-share-oriented options matching browser policy as closely as the SDK allows.
- Diagnostics snapshot reports `shareActive: true` when sharing is active but `shareStats` is null.
- Diagnostics UI/export shows `Sharing, waiting for stats` in that state.
- Diagnostics lifecycle timestamps use `shareActive`, not `shareStats !== null`.
- Keepalive writes reuse the cached decoded source and do not call the decode path after the first real Rust frame.
- A new Rust sequence replaces the cached decoded source and closes/releases the previous one.
- `stopNativeCapture()` and source replacement stop keepalive and release cached decoded resources.
- Diagnostics export includes the new cadence fields when sender stats are present or absent.
- Low native new-sequence FPS triggers the Windows GDI polling retry path without changing publish options.

### Rust Tests

- `ScreenShareConfig` preset tests prove:
  - `low = 1080p60 JPEG 85`
  - `high = 1440p30 JPEG 92`
  - `max = 1440p60 JPEG 95`
- Invalid preset behavior remains unchanged.
- Native bridge cadence diagnostics count raw frame arrivals, emitted frames, throttle drops, and encode timing.
- Windows GDI polling target FPS follows `ScreenShareConfig.max_fps()`.

## Out Of Scope

- Do not remove or rewrite the JPEG/base64 pipeline.
- Do not disable `dynacast` or `adaptiveStream`.
- Do not invent fake sender stats.
- Do not create unrelated media contract docs.
- Do not replace the LiveKit JS publish path with Rust publishing.
- Do not rewrite the whole JPEG/base64 bridge in this pass unless diagnostics prove real-frame decode remains the bottleneck after keepalive decode removal.

## Manual Verification Commands

Per repository instructions, agents should not run verification commands automatically. After implementation, the user should run:

```powershell
npm.cmd run test --workspace clients/wavis-gui
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Privacy Gate Reminder

Before any commit or PR, inspect staged, modified, and untracked files. This public repository must not commit secrets, private operational notes, AI scratchpads, internal reports, or other local-only artifacts.
