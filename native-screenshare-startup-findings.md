# Native Screenshare Startup Findings

## Current Observation

- Discord, OBS, and Google Meet can share on the affected Windows PC, so this is not evidence of a broad OS-level screen sharing failure.
- Wavis uses a custom native bridge: Rust capture -> RGBA processing -> JPEG/base64 -> shared latest frame -> Tauri IPC poll -> JS decode -> generated track -> LiveKit publish.
- WGC/native capture startup is now good enough to produce and publish a usable screen-share track.
- Voice remains functional after share start: remote participants can still hear the user.
- Remote participants can see the shared screen, so the active failure is not WGC session creation, first-frame readiness, Tauri polling startup, or LiveKit audio/video delivery.
- The remaining failure is local Wavis app responsiveness after the share starts. The local app becomes completely unresponsive, preventing the user from reaching in-app bug reporting or log capture.
- Terminal logs are currently the only reachable diagnostics during this failure mode.

## Stage Tracker

| Stage | Evidence to capture | Current status |
| --- | --- | --- |
| WGC/GDI capture | backend, source kind, item dimensions, adapter, raw frame latency, readback failures | Instrumented in `WindowsNativeCaptureDiagnostics` |
| Rust processing | cap/downscale, RGBA-to-RGB, JPEG encode, base64 encode, latest-frame write, raw-to-pollable total | Instrumented for first pollable frame |
| Pollable frame boundary | `frames_buffered`, first latest-frame write, `screen_share_start_source` readiness | `frames_buffered` now means latest frame was written |
| Tauri IPC polling | poll calls, misses, hits, first poll hit, latest seq | Instrumented in `screen_share_poll_frame` |
| JS decode | first poll frame, first decode start, first decoded/queued frame | Instrumented in `LiveKitModule.startNativeCapture()` |
| Publish | publish start/done and failure reason | Existing leak stages retained |

## Startup Contract

`screen_share_start_source` should return success only after Rust has produced a pollable first frame in `latest_frame`. JS starts the generator/publish path after that command resolves, then immediately polls a known-ready frame.

## Environment Correlation

Windows startup diagnostics collect best-effort context only around native share start:

- CPU and Wavis process-tree RSS/CPU.
- WebView-related process count.
- DXGI adapter metadata and memory where available.
- Selected backend, source kind, selected dimensions, target FPS, JPEG quality.
- Best-effort video driver provider/version/date through a bounded Windows query.
- Common capture/overlay process names if present.

Diagnostics are correlation data and must not block startup except for the explicit first-pollable-frame readiness gate.

## Previous Startup Root Cause (Resolved)

- The wrong-thread WGC startup issue is fixed.
- The bottleneck is first-frame Rust processing for a 3440x1440 input at the high preset.
- Observed first-frame timing: downscale about `935ms`, RGBA-to-RGB about `137ms`, JPEG encode about `944ms`, base64 about `5ms`, for `raw_to_pollable_ms=2135`.
- A flat `2500ms` WGC readiness timeout can fire just before or while the first pollable JPEG/base64 frame is being written.
- Startup readiness should be phase-aware: setup/session errors return immediately, no raw frame after `StartCapture` remains a distinct capture-start failure, and once a raw frame or buffered JPEG is observed the wait should switch to a longer first-frame processing budget, default `7500ms` for WGC.
- Unsupported cursor/border configuration errors such as `cursor_border_config_error: 0x80004002` are diagnostics noise and should not become the primary `firstError`.

## Current Hypotheses

1. Main-thread or event-loop starvation after native screen-share publication is preventing the local UI from processing input and rendering.
2. Frontend polling, decode, or render loop load may be overwhelming the WebView once the share is active.
3. Tauri/WebView event-loop interaction during native screen-share publication may be blocking local responsiveness even though media continues to flow remotely.
4. Terminal-only diagnostics are required for the next pass because in-app bug reporting is unavailable during the hang.

## 2026-06-10 Post-Startup UI Hang

- The failure has moved past capture startup and LiveKit publish.
- Voice remains usable and remote participants can hear the user.
- Remote participants can see the shared screen.
- The local Wavis app becomes completely unresponsive after share start.
- In-app bug reporting and log capture cannot be reached during the hang, so terminal logs are currently the only reachable diagnostics.
- `SetIsBorderRequired failed: No such interface supported (0x80004002)` still appears, but this remains nonfatal diagnostics noise and should not be treated as a blocking startup or publish error.
- Tao event-loop warnings occurred around the unresponsive period:
  - `NewEvents emitted without explicit RedrawEventsCleared`
  - `RedrawEventsCleared emitted without explicit MainEventsCleared`
- Working conclusion: focus the next investigation on post-start local UI/event-loop responsiveness, especially main-thread starvation, frontend polling/decode/render loop pressure, and Tauri/WebView event-loop behavior during native screen-share publication.

## 2026-06-10 Stabilization Patch

- Current mitigation targets post-start UI/event-loop pressure, not WGC startup or LiveKit publish.
- The Windows native JS bridge now applies backpressure: polling is non-overlapping, pauses while JS decode/write work is in flight, and resumes by polling Rust for the latest frame.
- The Windows native bridge transport cadence is capped at `min(publish_fps, 15)`, and Rust uses the same cap before downscale/JPEG/base64 work so it does not process frames the WebView will not consume.
- `SetIsBorderRequired failed: No such interface supported (0x80004002)` remains nonfatal diagnostics noise.
- Replacing the JPEG/base64 latest-frame bridge with a binary/raw transport remains a future optimization, not part of this stabilization patch.

## 2026-06-09 WGC Startup Root Cause (Resolved)

- New diagnostics show WGC now reaches `frame_pool_create_done`, then fails at `capture_session_create_error`.
- The failing HRESULT is `0x8001010E`, which indicates wrong-thread marshaling in this WinRT path.
- Root cause: `GraphicsCaptureItem` was created before spawning `win-capture`, then used by that capture thread to create the session. Treat the item as thread-affine in this code path and create it on the same thread as the frame pool/session.
