# Native Screenshare Quality Implementation Plan

## Goal

Improve the native custom screen-share quality path and diagnostics without a large architecture rewrite.

This patch should keep the current JPEG/base64 frame pipeline and leave `dynacast` / `adaptiveStream` enabled. The focus is to fix quality state propagation, align Rust and JS preset semantics, improve native LiveKit publish options, and make diagnostics distinguish active sharing from missing sender stats.

Reference investigation: [native-screenshare-quality-findings.md](native-screenshare-quality-findings.md).

## Progress Tracker

- [x] Persist selected screen-share quality before start.
- [x] Prevent manual `max` from being downgraded by post-start profile initialization.
- [x] Sync selected quality into Rust before native capture starts.
- [x] Align Rust capture presets with the canonical policy.
- [x] Improve native generated-track publish options to reuse the screen-share policy builder.
- [x] Fix diagnostics so active sharing is distinct from missing sender stats.
- [ ] Run manual verification commands.

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

## Test Tasks

### GUI / TypeScript Tests

- Selecting `max` before starting a custom native share persists into `LiveKitModule.currentQuality`.
- Windows native custom share invokes `media_set_screen_share_quality` before `screen_share_start_source`.
- Post-start profile initialization does not downgrade persisted manual `max` to `high`.
- Native `publishTrack()` receives screen-share-oriented options matching browser policy as closely as the SDK allows.
- Diagnostics snapshot reports `shareActive: true` when sharing is active but `shareStats` is null.
- Diagnostics UI/export shows `Sharing, waiting for stats` in that state.
- Diagnostics lifecycle timestamps use `shareActive`, not `shareStats !== null`.

### Rust Tests

- `ScreenShareConfig` preset tests prove:
  - `low = 1080p60 JPEG 85`
  - `high = 1440p30 JPEG 92`
  - `max = 1440p60 JPEG 95`
- Invalid preset behavior remains unchanged.

## Out Of Scope

- Do not remove or rewrite the JPEG/base64 pipeline.
- Do not disable `dynacast` or `adaptiveStream`.
- Do not invent fake sender stats.
- Do not create unrelated media contract docs.

## Manual Verification Commands

Per repository instructions, agents should not run verification commands automatically. After implementation, the user should run:

```powershell
npm.cmd run test --workspace clients/wavis-gui
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Privacy Gate Reminder

Before any commit or PR, inspect staged, modified, and untracked files. This public repository must not commit secrets, private operational notes, AI scratchpads, internal reports, or other local-only artifacts.
