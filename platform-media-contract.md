# Wavis Platform Media Contract

This document defines the platform-neutral behavior expected from Wavis media
features across Windows, macOS, and Linux. Implementations may use different
native APIs per operating system, but the observable product behavior should be
consistent.

The contract applies to voice, screen sharing, system audio sharing, media
permissions, LiveKit/WebRTC publishing, media quality controls, diagnostics,
and feature fallbacks.

## Goals

- Give every media feature one product-level behavior contract.
- Keep platform-specific capture and audio APIs behind narrow adapters.
- Normalize errors, lifecycle states, telemetry, and user-visible behavior.
- Make Linux behavior explicit instead of treating it as a best-effort variant
  of Windows or macOS.
- Catch platform drift before features are merged.

## Non-Goals

- The same implementation on every platform.
- Perfect feature parity when the operating system does not expose equivalent
  APIs.
- Hiding meaningful platform limitations from users or diagnostics.

## Design Rule

New media features must define the desired Wavis behavior first, then map that
behavior to each platform backend.

The frontend should depend on Wavis-level media commands and events, not on
platform details such as PipeWire, WASAPI, ScreenCaptureKit, X11, or a specific
LiveKit SDK path.

When implementation reaches a point where behavior may need to differ by
platform, leave a short code comment at that boundary. The comment should name
the platform-sensitive assumption, describe the Wavis-level contract that must
still hold, and identify what another platform implementation needs to verify or
replace.

Example:

```rust
// Platform note: this path uses WASAPI loopback for Windows system audio.
// Linux must provide the same Wavis contract through PipeWire/PulseAudio:
// one screen-share audio track, no duplicate capture, normalized cleanup.
```

These comments should be used at platform boundaries, capability assumptions,
permission flows, capture source selection, codec/encoder decisions, and cleanup
logic. Avoid comments that only restate the code; the purpose is to preserve
cross-platform intent for the next platform implementer.

## Feature Contract Template

Every new media feature should document:

- User-visible behavior.
- Supported platforms.
- Required platform capabilities.
- Runtime capability detection.
- Permission flow.
- Start, active, degraded, failed, and stop behavior.
- Error categories returned to the frontend.
- Cleanup guarantees.
- Telemetry emitted.
- Automated tests.
- Manual QA matrix.
- Platform-sensitive code comments added at implementation boundaries.

## Common Media States

Media features should use the same lifecycle vocabulary across platforms:

| State | Meaning |
| --- | --- |
| `idle` | Feature is not active and no start is in progress. |
| `requesting_permission` | Wavis is waiting for an OS, portal, or browser permission decision. |
| `starting` | Permission is available and the capture/publish pipeline is being created. |
| `active` | Media is being captured and published or played as intended. |
| `degraded` | Feature is running with reduced capability, quality, or fallback behavior. |
| `failed` | Feature could not start or continue. |
| `stopping` | Wavis is tearing down tracks, devices, timers, subprocesses, or OS sessions. |

State transitions should be idempotent where practical. Calling stop on an
already stopped feature should succeed or return a normalized no-op result, not
leave the app in an inconsistent state.

## Error Categories

Platform-specific failures should be mapped into stable error categories before
they reach the UI:

| Category | Meaning |
| --- | --- |
| `permission_denied` | The OS, portal, browser, or user policy denied access. |
| `user_cancelled` | The user intentionally cancelled a picker or permission dialog. |
| `unsupported` | The feature is unavailable on this platform/session. |
| `missing_dependency` | A required runtime component is unavailable. |
| `device_busy` | A capture or audio device is already in use. |
| `source_unavailable` | The selected window, monitor, or device disappeared. |
| `capture_failed` | Capture started but failed to produce usable media. |
| `publish_failed` | Local capture worked, but LiveKit/WebRTC publishing failed. |
| `network_failed` | Media transport failed due to ICE, TURN, SFU, or connection state. |
| `internal_error` | Unexpected application error. |

Error payloads should include a stable category and a diagnostic detail string.
The UI should key behavior off the category, not off platform-specific message
text.

## Telemetry And Diagnostics

Every media session should log enough information to debug platform behavior:

- OS, version, architecture.
- App version and build channel.
- Media backend name.
- Linux session details when applicable: Wayland/X11, desktop environment,
  compositor hints, portal availability, PipeWire availability.
- Permission result.
- Selected source type: monitor, window, camera, microphone, system audio.
- Capture dimensions, FPS, frame cadence, and dropped frame counts.
- Codec, encoder path when known, target bitrate, actual bitrate.
- RTP/RTCP stats: RTT, jitter, packet loss, NACK count, PLI count.
- LiveKit/WebRTC connection state and reconnect events.
- Cleanup outcome on stop.

Telemetry should make fallback behavior explicit. For example, Linux screen
sharing should report whether it used PipeWire/portal or X11 fallback.

## Platform Backend Expectations

### Windows

Expected backend choices:

- Screen capture: Windows Graphics Capture or the current supported capture
  path.
- System audio: WASAPI loopback or a clearly documented native equivalent.
- Microphone: WASAPI or CPAL-backed native capture.
- Media transport: LiveKit/WebRTC path selected by the app.

Important checks:

- Avoid double-capturing system audio through both display capture and native
  loopback.
- Stop must release capture sessions and remove OS capture indicators.
- Device changes should be detected or surfaced as recoverable errors.

### macOS

Expected backend choices:

- Screen capture: ScreenCaptureKit where native capture is used.
- System audio: Process tap, virtual device, or documented fallback.
- Microphone: CoreAudio or CPAL-backed native capture.
- Media transport: LiveKit/WebRTC path selected by the app.

Important checks:

- Handle Screen Recording and Microphone permissions explicitly.
- Permission changes may require app restart or a new capture session.
- Stop must release capture sessions and OS indicators.

### Linux

Linux must be treated as multiple environments, not as one uniform platform.

Preferred screen capture order:

1. Wayland: XDG Desktop Portal with PipeWire.
2. X11: X11 capture fallback.
3. Unsupported/degraded state with diagnostics when neither path is available.

Expected backend choices:

- Wayland screen capture: XDG Desktop Portal plus PipeWire.
- X11 screen capture: X11 fallback capture.
- System audio: PipeWire-Pulse or PulseAudio-compatible path.
- Microphone: CPAL/PulseAudio/PipeWire-backed native capture.
- Media transport: Rust/native LiveKit path when the webview cannot provide
  reliable WebRTC support.

Important checks:

- Detect Wayland vs X11 at runtime.
- Detect portal and PipeWire availability.
- Treat user cancellation in the portal picker as `user_cancelled`.
- Treat missing portal or PipeWire as `missing_dependency` or `unsupported`,
  depending on whether another backend can run.
- Account for compositor differences such as GNOME, KDE, Hyprland, Sway, and
  XWayland sessions.
- Test multi-monitor layout, fractional scaling, and source disappearance.

## Screen Sharing Contract

Screen sharing must provide the same Wavis-level behavior on every supported
platform:

- User can start sharing a monitor or window when the platform supports it.
- User can stop sharing from Wavis even if the OS also exposes a stop control.
- Remote participants receive a screen-share track or a normalized failure
  event.
- Local UI receives active/degraded/failed state updates.
- Quality controls apply without restarting capture when the backend supports
  live adjustment.
- Restarting or changing source should preserve user-selected quality settings.
- Stopping must unpublish tracks and release OS capture resources.

If a platform cannot support source selection, system audio, or live quality
changes, the feature should enter `degraded` with a specific capability reason.

## System Audio Sharing Contract

System audio sharing must be modeled separately from screen video capture.

Rules:

- Do not rely on display capture audio when a native audio route is required to
  avoid echo or inconsistent behavior.
- Avoid publishing duplicate audio tracks for the same screen share.
- Muting, volume control, and cleanup must behave consistently across platforms.
- If echo cancellation or app-exclusion is unavailable, report a degraded mode.

## Capability Detection

Feature availability should be determined at runtime and exposed to the
frontend as structured capability data.

Suggested fields:

```ts
type MediaCapabilities = {
  platform: 'windows' | 'macos' | 'linux';
  session?: 'wayland' | 'x11';
  screenShare: {
    supported: boolean;
    backend: string | null;
    sourceSelection: boolean;
    windowCapture: boolean;
    monitorCapture: boolean;
    liveQualityChange: boolean;
    reason?: string;
  };
  systemAudio: {
    supported: boolean;
    backend: string | null;
    appExclusion: boolean;
    reason?: string;
  };
};
```

The UI should use capability data to decide what controls to show or disable.
It should not infer support from OS names alone.

## Testing Standard

Each cross-platform media feature should include:

- Unit tests for normalized state transitions and error mapping.
- Tests for cleanup idempotency.
- Tests for frontend behavior against the Wavis-level API.
- Platform-specific tests for backend selection logic.
- Manual QA steps for Windows, macOS, Linux Wayland, and Linux X11.

Linux manual QA should include:

- GNOME Wayland.
- KDE Wayland when available.
- At least one wlroots compositor when supported, such as Sway or Hyprland.
- X11 session.
- Single monitor and multi-monitor.
- Fractional scaling.
- Portal cancellation.
- Missing portal/PipeWire dependency behavior where practical.

## Merge Gate For New Media Features

Before merging a new media feature, answer:

1. What is the platform-neutral Wavis behavior?
2. Which backend implements it on Windows, macOS, Linux Wayland, and Linux X11?
3. What capability data does the frontend receive?
4. What normalized errors can be returned?
5. What degraded modes exist?
6. What resources are cleaned up on stop, reconnect, source change, and app
   exit?
7. What telemetry identifies backend, permission result, codec, FPS, bitrate,
   packet loss, and failure reason?
8. Which automated tests cover the contract?
9. Which manual QA environments were tested?
10. Where did the implementation require platform-sensitive assumptions, and
    are those assumptions documented with code comments?

If these questions do not have clear answers, the feature is not ready for
cross-platform release.

## Recommended Code Shape

Prefer this dependency direction:

```text
React/UI
  -> Wavis media API
    -> platform-neutral command/event contract
      -> platform adapter
        -> OS/native API or LiveKit/WebRTC SDK
```

Avoid this shape:

```text
React/UI
  -> direct platform checks
  -> direct backend-specific behavior
  -> different user-visible semantics per OS
```

Platform-specific code is expected. Platform-specific user experience should be
intentional, documented, and represented as capability or degraded-mode data.
