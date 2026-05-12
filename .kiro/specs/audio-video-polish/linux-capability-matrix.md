# Linux Capability Matrix

Source of truth for W3 runtime gating. This file documents the current Linux native share path as it exists in code today:

- Screen video is captured through `clients/wavis-gui/src-tauri/src/screen_capture.rs` and `clients/wavis-gui/src-tauri/src/screen_capture/pipewire_capture.rs`, with PipeWire/portal first and X11 fallback only where that path exists.
- System audio is captured separately through `clients/wavis-gui/src-tauri/src/audio_capture/platform/linux.rs` via PulseAudio or PipeWire-Pulse monitor sources.
- No row claims that the portal stack "handles everything"; video and audio are separate pipelines with separate failure modes.

## Runtime Matrix

| Desktop env | Compositor | Screen video (Video_Capture_Linux) | System audio (Audio_Capture_Linux) | Status | User message | Notes |
|---|---|---|---|---|---|---|
| gnome | wayland | supported - PipeWire portal video path; no X11 fallback on pure Wayland. | degraded - PulseAudio or PipeWire-Pulse monitor capture; startup aborts if loopback exclusion cannot isolate Wavis playback. | degraded | Linux native share on GNOME/Wayland is degraded: remote mic and system-audio still share one subscriber volume slot on Linux. | Video and audio are separate native paths. The current degradation is the known Linux subscriber control limitation, not a portal limitation. |
| kde | wayland | supported - PipeWire portal video path; no X11 fallback on pure Wayland. | degraded - PulseAudio or PipeWire-Pulse monitor capture; startup aborts if loopback exclusion cannot isolate Wavis playback. | degraded | Linux native share on KDE/Wayland is degraded: remote mic and system-audio still share one subscriber volume slot on Linux. | Same split as GNOME/Wayland: PipeWire portal video plus PulseAudio monitor audio. |
| sway | wayland | unsupported - this build does not promise wlroots or Sway portal coverage in the checked-in matrix. | unsupported - audio is blocked alongside the unsupported video path so share start fails early instead of half-starting. | unsupported | Linux native share is not supported on Sway/Wayland in the checked-in Wavis matrix yet; use GNOME, KDE, or an X11 session instead. | Conservative gate: fail early instead of silently falling through to a partial native share path on unvalidated wlroots setups. |
| gnome | x11 | supported - PipeWire portal video is tried first; X11 capture is the native fallback when the portal path is unavailable. | degraded - PulseAudio or PipeWire-Pulse monitor capture; startup aborts if loopback exclusion cannot isolate Wavis playback. | degraded | Linux native share on GNOME/X11 is degraded: remote mic and system-audio still share one subscriber volume slot on Linux. | X11 can succeed without the Wayland portal path, but audio remains a separate PulseAudio monitor capture path. |
| kde | x11 | supported - PipeWire portal video is tried first; X11 capture is the native fallback when the portal path is unavailable. | degraded - PulseAudio or PipeWire-Pulse monitor capture; startup aborts if loopback exclusion cannot isolate Wavis playback. | degraded | Linux native share on KDE/X11 is degraded: remote mic and system-audio still share one subscriber volume slot on Linux. | The share path is still split; X11 fallback only affects video capture. |
| xfce | x11 | supported - PipeWire portal video is tried first; X11 capture is the native fallback when the portal path is unavailable. | degraded - PulseAudio or PipeWire-Pulse monitor capture; startup aborts if loopback exclusion cannot isolate Wavis playback. | degraded | Linux native share on XFCE/X11 is degraded: remote mic and system-audio still share one subscriber volume slot on Linux. | XFCE follows the same X11 fallback video path and PulseAudio monitor audio path as the other X11 rows. |

## Known Limitation

| Desktop env | Compositor | Screen video (Video_Capture_Linux) | System audio (Audio_Capture_Linux) | Status | Notes |
|---|---|---|---|---|---|
| linux-native-subscriber-path | any | unchanged | degraded - subscriber mic and system-audio are controlled together, not independently. | degraded | This is the scoped-out R4 limitation from `clients/shared/src/peer_volumes.rs` and `LiveKitConnection::on_audio_frame(...)`: Linux native subscribers still have one mute or volume slot per participant instead of one slot per participant plus track kind. |
