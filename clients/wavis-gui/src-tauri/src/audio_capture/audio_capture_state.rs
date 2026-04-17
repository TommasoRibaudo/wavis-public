//! Capture session state types for all supported platforms.
//!
//! Defines AudioCaptureState (the Tauri-managed handle) and the platform-specific
//! handle types it wraps. Each handle owns the live resources for one capture
//! session: threads, stop flags, PulseAudio modules, COM objects, or ObjC streams.
//!
//! Invariants:
//! - active is None when idle, Some only while capture is running.
//! - All teardown must be idempotent (section 4.3): stopping when active is None is a no-op.

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::sync::{atomic::AtomicBool, Arc, Mutex};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::thread::JoinHandle;

#[cfg(target_os = "macos")]
use std::sync::{atomic::AtomicBool, mpsc::SyncSender, Arc, Mutex};

/// Managed Tauri state for audio capture.
///
/// Holds an optional platform-specific capture handle behind a `Mutex`. When
/// `None`, no audio capture is active. When `Some`, the handle owns the
/// resources needed for cleanup on that platform.
#[cfg(target_os = "linux")]
pub struct AudioCaptureState {
    pub(crate) active: Mutex<Option<AudioCaptureHandle>>,
}

#[cfg(target_os = "linux")]
impl AudioCaptureState {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }
}

/// Internal handle for an active audio capture session (Linux / PulseAudio).
///
/// Owns the capture thread and all bookkeeping needed to cleanly tear down
/// loopback exclusion on stop.
#[cfg(target_os = "linux")]
pub(crate) struct AudioCaptureHandle {
    /// PulseAudio capture thread join handle.
    pub(crate) pa_thread: JoinHandle<()>,
    /// Signal to stop the capture loop.
    pub(crate) stop_flag: Arc<AtomicBool>,
    /// Source ID being captured.
    #[allow(dead_code)]
    pub(crate) source_id: String,
    /// Module index of the loaded null sink (for cleanup).
    pub(crate) null_sink_module: Option<u32>,
    /// Module index of the loopback module (for cleanup).
    pub(crate) loopback_module: Option<u32>,
    /// PID used to identify the Wavis sink-input for loopback exclusion.
    #[allow(dead_code)]
    pub(crate) matched_pid: u32,
    /// Sink-inputs moved to the capture sink (for restore on stop).
    pub(crate) moved_inputs: Vec<MovedSinkInput>,
    /// Original default sink name (restored on teardown).
    pub(crate) original_default_sink: Option<String>,
}

/// Windows WASAPI loopback capture state.
#[cfg(target_os = "windows")]
pub(crate) struct WasapiCaptureHandle {
    /// WASAPI capture thread join handle.
    pub(crate) capture_thread: JoinHandle<()>,
    /// Signal to stop the capture loop.
    pub(crate) stop_flag: Arc<AtomicBool>,
    /// Source ID being captured (used by teardown/diagnostics).
    #[allow(dead_code)]
    pub(crate) source_id: String,
    /// Whether process-specific loopback exclusion is active (requires Windows 10 21H1+).
    /// Retained for future diagnostics/telemetry.
    #[allow(dead_code)]
    pub(crate) loopback_exclusion_active: bool,
}

/// Windows audio capture state - holds an optional `WasapiCaptureHandle`.
#[cfg(target_os = "windows")]
pub struct AudioCaptureState {
    pub(crate) active: Mutex<Option<WasapiCaptureHandle>>,
}

#[cfg(target_os = "windows")]
impl AudioCaptureState {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }
}

/// Internal handle for an active ScreenCaptureKit audio session.
///
/// Owns the SCStream and keeps the ObjC delegate alive for the full lifetime
/// of the stream. The delegate is stored type-erased as `Box<dyn Any>` so
/// this file has no dependency on the `AudioOutputHandler` ObjC class defined
/// in `platform/macos.rs`.
#[cfg(target_os = "macos")]
pub(crate) struct ScCaptureHandle {
    /// The active ScreenCaptureKit stream.
    pub(crate) stream: objc2::rc::Retained<objc2_screen_capture_kit::SCStream>,
    /// Signal to stop the capture loop.
    pub(crate) stop_flag: Arc<AtomicBool>,
    /// Source ID retained for diagnostics.
    #[allow(dead_code)]
    pub(crate) source_id: String,
    /// Keeps the `AudioOutputHandler` ObjC delegate alive. Stored as `Box<dyn
    /// Any>` to avoid a cross-module dependency on the concrete delegate type.
    pub(crate) _handler: Box<dyn std::any::Any>,
}

// SAFETY: SCStream and the boxed AudioOutputHandler are ref-counted ObjC
// objects; thread-safe to move across boundaries (only one thread mutates at a
// time).
#[cfg(target_os = "macos")]
unsafe impl Send for ScCaptureHandle {}

/// Shared context between `audio_share_start_tap` and the CoreAudio IOProc
/// callback. Heap-allocated and kept alive via `Arc` for the lifetime of the
/// IOProc.
#[cfg(target_os = "macos")]
pub(crate) struct IoProcCtx {
    pub(crate) app: tauri::AppHandle,
    /// i16 PCM accumulation buffer — drained in 960-sample frames.
    pub(crate) accum: std::sync::Mutex<Vec<i16>>,
    pub(crate) stop_flag: Arc<AtomicBool>,
    /// One-shot channel: the IOProc takes the sender and fires it on the first
    /// audio callback to confirm the tap is live. `None` after first use.
    pub(crate) first_frame_tx: std::sync::Mutex<Option<SyncSender<()>>>,
    pub(crate) native_rate: f64,
    pub(crate) native_channels: u32,
}

// SAFETY: tauri::AppHandle is Send; all other fields are Send.
#[cfg(target_os = "macos")]
unsafe impl Send for IoProcCtx {}
#[cfg(target_os = "macos")]
unsafe impl Sync for IoProcCtx {}

/// Internal handle for an active Core Audio process tap session (macOS 14.2+).
#[cfg(target_os = "macos")]
pub(crate) struct TapCaptureHandle {
    /// AudioObjectID for the tap.
    pub(crate) tap_id: u32,
    /// AudioDeviceIOProcID (opaque CoreAudio handle).
    pub(crate) proc_id: *mut std::ffi::c_void,
    /// Signal to stop the capture loop.
    pub(crate) stop_flag: Arc<AtomicBool>,
    #[allow(dead_code)]
    pub(crate) source_id: String,
    /// Keeps IoProcCtx alive for the entire lifetime of the IOProc callback.
    pub(crate) _ctx: Arc<IoProcCtx>,
}

// SAFETY: proc_id is an opaque CoreAudio handle valid until
// AudioDeviceDestroyIOProcID is called on the stop path.
#[cfg(target_os = "macos")]
unsafe impl Send for TapCaptureHandle {}

/// Routing resources for the virtual-device capture fallback.
#[cfg(target_os = "macos")]
pub(crate) struct VirtualDeviceRoutingState {
    /// System output device that was active before Wavis swapped in the
    /// temporary multi-output bridge.
    pub(crate) original_default_output: u32,
    /// Temporary multi-output aggregate device created for the share session.
    pub(crate) aggregate_device_id: u32,
    /// AudioQueueRef for the BlackHole loopback input capture. The aggregate
    /// routes audio passively (no IOProc); this queue reads from BlackHole's
    /// input stream independently. Null when no queue is active.
    pub(crate) audio_queue: *mut std::ffi::c_void,
    /// Shared stop flag used by the AudioQueue callback.
    pub(crate) stop_flag: Arc<AtomicBool>,
}

/// Internal handle for an active virtual-device capture session.
#[cfg(target_os = "macos")]
pub(crate) struct VirtualDeviceCaptureHandle {
    /// Owns the temporary routing state for the lifetime of the session.
    pub(crate) routing_state: VirtualDeviceRoutingState,
    /// Source ID retained for diagnostics.
    #[allow(dead_code)]
    pub(crate) source_id: String,
    /// Keeps IoProcCtx alive for the entire lifetime of the AudioQueue callback.
    pub(crate) _ctx: Arc<IoProcCtx>,
}

// SAFETY: AudioQueueRef is an opaque CoreAudio handle that is safe to move
// between threads; only one thread (the teardown path) disposes it.
// IoProcCtx is already Send + Sync.
#[cfg(target_os = "macos")]
unsafe impl Send for VirtualDeviceCaptureHandle {}

/// Active audio session: either ScreenCaptureKit (macOS 12.3–14.1) or a
/// Core Audio process tap (macOS 14.2+).
#[cfg(target_os = "macos")]
pub(crate) enum MacAudioHandle {
    Sck(ScCaptureHandle),
    Tap(TapCaptureHandle),
    VirtualDevice(VirtualDeviceCaptureHandle),
}

/// macOS audio capture state - holds an optional handle for the active audio
/// session. On macOS 14.2+ the handle is a Core Audio process tap
/// (`MacAudioHandle::Tap`); on macOS 12.3-14.1 it is a ScreenCaptureKit
/// session (`MacAudioHandle::Sck`).
#[cfg(target_os = "macos")]
pub struct AudioCaptureState {
    pub(crate) active: Mutex<Option<MacAudioHandle>>,
}

#[cfg(target_os = "macos")]
impl AudioCaptureState {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }
}

/// Stub for platforms without audio capture support.
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub struct AudioCaptureState;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
impl AudioCaptureState {
    pub fn new() -> Self {
        Self
    }
}

/// Result of `audio_share_start` - returned on successful capture start.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudioShareStartResult {
    /// Whether audio isolation is available and active for this capture
    /// session.
    pub loopback_exclusion_available: bool,
    /// CoreAudio UID for the real output device when virtual-device routing is
    /// active. Existing tap/SCK paths return `None`.
    pub real_output_device_id: Option<String>,
    /// Human-readable name of the real output device (e.g. "MacBook Pro Speakers").
    /// Used as a label-based fallback when the CoreAudio UID does not match any
    /// browser `deviceId` / `groupId` in `enumerateDevices()` — which is the
    /// common case on the bypass path (Multi-Output Device already contains BlackHole).
    pub real_output_device_name: Option<String>,
    /// When true, the capture path cannot exclude room audio at the OS level
    /// (macOS virtual-device path: the Wavis Bridge routes all audio through
    /// BlackHole, and WebKit's AudioContext.setSinkId is unavailable).
    /// The JS side must mute local LiveKit playback to prevent the viewer from
    /// hearing their own voice in loopback.
    pub requires_mute_for_echo_prevention: bool,
}

/// A single sink-input that was moved to the null sink for loopback exclusion.
#[derive(Clone)]
#[cfg(target_os = "linux")]
pub(crate) struct MovedSinkInput {
    /// PulseAudio sink-input index.
    pub(crate) index: u32,
    /// Original sink index (as string) for restore.
    pub(crate) original_sink: Option<String>,
}
