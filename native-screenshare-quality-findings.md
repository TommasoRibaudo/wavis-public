# Native Screenshare Quality Findings

## Context

This note documents the current investigation into why the native custom screen-share path can look blurrier or less smooth than the older browser/Tauri `getDisplayMedia()` path.

The current Windows custom path is intentionally different from browser capture:

- Rust enumerates and captures the selected screen/window.
- Rust caps resolution/FPS, strips RGBA to RGB, JPEG-encodes the frame, base64-encodes it, and stores the latest frame.
- JS polls `screen_share_poll_frame`, decodes the JPEG/base64 payload, writes frames through `MediaStreamTrackGenerator` or a hidden canvas fallback, then publishes that generated track through the LiveKit JS SDK.
- The browser fallback gives LiveKit a real browser `MediaStreamTrack` from `getDisplayMedia()`, avoiding the intermediate JPEG/base64/decode/copy stage.

`platform-media-contract.md` is referenced by `AGENTS.md`, but it is not present at the repository root during this inspection.

## Ranked Probable Causes

1. **Native capture uses an extra lossy and CPU-heavy frame pipeline.**
   This is the strongest explanation for both blur and stutter. The native path performs capture -> RGBA processing/downscale -> JPEG encode -> base64 encode -> JS polling -> JPEG decode -> `ImageBitmap`/`VideoFrame` or canvas -> LiveKit encode. Text and UI edges are especially vulnerable because JPEG is lossy before LiveKit applies its own video encoding.

2. **The selected UI quality is not reliably persisted before share start.**
   `LiveKitModule.setScreenShareQuality()` currently returns early when there is no active screen-share publication. That means selecting a quality before the native share is published can leave `currentQuality` at its default value (`high`) instead of the user-selected value. Native capture startup then derives publish FPS/bitrate from stale JS state.

3. **JS quality selection and Rust capture quality are separate state machines.**
   Rust exposes `media_set_screen_share_quality()` and `ScreenShareConfig`, but the Windows custom-share start path does not clearly sync the selected preset into Rust immediately before `screen_share_start_source`. If Rust keeps its default config, the capture-side cap and JPEG quality can diverge from the UI-selected profile even when LiveKit publish options are correct.

4. **Native publish options still do not fully mirror the browser screen-share policy.**
   Browser screen share uses `buildSdkScreenSharePublishOptions()` with `screenShareEncoding`, `screenShareSimulcastLayers`, codec policy, and `degradationPreference`. `startNativeCapture()` prepares similar values, but the actual native `publishTrack()` call maps the bitrate/FPS through `videoEncoding` and does not pass the complete `buildSdkScreenSharePublishOptions()` result. This risks LiveKit treating the generated native track more like generic video than a detail-preserving screen share.

5. **Current Rust presets do not match the intended JS profile semantics.**
   The current Rust presets are:

   | Preset | Rust capture cap |
   |--------|------------------|
   | `low` | 1920x1080 @ 60fps, JPEG 85 |
   | `high` | 2560x1440 @ 60fps, JPEG 92 |
   | `max` | 2560x1440 @ 60fps, JPEG 95 |

   JS maps `low` to motion, `high` to detail, and `max` to detail at 60fps. The target policy should be made explicit, because a detail-oriented preset usually favors resolution and stability while a motion preset favors FPS. If `high/detail` is intended to be 1440p30 and `max` 1440p60, Rust currently runs `high` at 60fps and may spend CPU budget on frame rate instead of visual stability.

6. **Native sender stats/adaptive handling may not be equivalent to browser capture.**
   The browser path has established sender stats polling and adaptive quality behavior around screen share. The native path has extensive native-start diagnostics and a LiveKit publish step, but parity should be verified explicitly: outbound frame size, FPS, bitrate, quality limitation reason, PLI/NACK, loss, and adaptive tier should be collected for both paths at the same selected preset.

7. **Dynacast/adaptiveStream remain secondary suspects.**
   `LIVEKIT_ROOM_OPTIONS` currently enables `adaptiveStream: true` and `dynacast: true`. Existing repo memory notes that small rooms previously saw aggressive screen-share downgrades with dynacast. This is probably secondary to the native JPEG/base64 pipeline, but it remains a useful isolation switch if quality remains poor after native capture and publish parity are fixed.

8. **There is a platform-branch risk around native publication startup.**
   The custom-share orchestration comments describe the Windows LiveKit JS path as Rust capture plus `startNativeCapture()`, but the visible condition near the current start logic should be audited carefully because comment and branch shape are not self-evidently aligned. Treat this as an implementation audit item before assuming all Windows starts publish through the intended native bridge.

## Ranked Fixes

1. **Persist selected quality even before a publication exists.**
   Update `LiveKitModule.setScreenShareQuality()` so it records `currentQuality = quality` before checking for an active publication. If a share is already active, continue applying constraints and reporting actual settings. If no share is active, store the selection for the next start.

2. **Sync selected quality into Rust before every native capture start.**
   Immediately before `screen_share_start_source`, call `media_set_screen_share_quality({ quality })` for Windows and Linux native capture paths. This should happen even if no active share publication exists yet. Add a focused test that selected `max` before start reaches both JS `currentQuality` and the Rust IPC command order.

3. **Make Rust and JS presets intentionally match.**
   Decide the canonical profile mapping and encode it in both layers. Recommended baseline:

   | Profile | Intended use | Capture/publish target |
   |---------|--------------|------------------------|
   | `low` / `motion` | smooth motion | 1080p60 |
   | `high` / `detail` | text/detail | 1440p30 |
   | `max` | detail plus motion | 1440p60 |

   Keep JPEG quality high enough for text while measuring encode time. If `high` remains 60fps, document that as an intentional tradeoff rather than an accidental mismatch.

4. **Publish native tracks with the same screen-share policy builder.**
   Reuse `buildSdkScreenSharePublishOptions()` in `startNativeCapture()` and include the screen-share-specific fields rather than reconstructing a partial options object. Preserve the native track `name`, `source`, and `stream`, but avoid falling back to camera-style `videoEncoding` unless LiveKit requires it for generated tracks.

5. **Compare native vs browser with outbound stats before deeper optimization.**
   For the same source and selected preset, record:

   - encoded width/height
   - FPS sent
   - outbound bitrate
   - quality limitation reason
   - PLI/NACK counts
   - packet loss
   - capture FPS
   - JS processed FPS
   - JPEG encode/decode time if available

   Native should match browser path dimensions/FPS/bitrate before quality can be considered fixed.

6. **Reduce or remove JPEG/base64 from the native hot path.**
   Short term: raise/confirm JPEG quality, avoid unnecessary downscale, and log per-frame encode/decode cost. Long term: prefer raw/shared-memory/WebCodecs-friendly transfer or a native LiveKit video-source/encoder path so screen content is not JPEG-compressed before WebRTC encoding.

7. **Re-test dynacast only after native parity fixes.**
   If native still degrades after quality sync and publish-option parity, run an A/B with `dynacast: false` or a screen-share-specific subscriber quality pin. This should be treated as an isolation test, not the first fix.

## Suggested Tests

Add or update tests for:

- Selecting `max` before starting share updates `LiveKitModule.currentQuality`.
- Windows native start calls `media_set_screen_share_quality` before `screen_share_start_source`.
- Native publish options include the same bitrate/FPS/degradation/simulcast policy as the browser screen-share path.
- Rust `ScreenShareConfig` presets match the canonical JS profile policy.
- Native capture diagnostics report the selected preset, Rust cap, JPEG quality, capture FPS, JS processed FPS, and outbound sender dimensions/bitrate.

## Verification

No automated verification commands were run for this documentation-only change. Per the repository instructions, verification commands are manual-run only. If code changes are made later, the user should run:

```powershell
cargo test --workspace
cargo clippy --workspace -- -D warnings
```
