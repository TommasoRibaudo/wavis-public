//! Linux screen capture abstraction layer.
//!
//! Defines the `ScreenCapture` trait and supporting types for platform-abstracted
//! screen capture on Linux. Implementations (PipeWire/portal, X11/XWayland) are
//! provided in submodules added by subsequent tasks.

#[cfg(target_os = "linux")]
pub mod codec_detect;
pub mod frame_processor;
#[cfg(target_os = "linux")]
pub mod pipewire_capture;
#[cfg(target_os = "linux")]
pub mod source_capture;
#[cfg(target_os = "windows")]
pub mod win_capture;
#[cfg(target_os = "linux")]
pub mod x11_capture;

use std::fmt;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

/// Error types for screen capture operations.
///
/// Each variant maps to a specific IPC result in the `screen_share_start` command:
/// - `UserCancelled` → `Ok(false)` (silent no-op)
/// - `PortalUnavailable` → triggers X11 fallback (not surfaced to JS directly)
/// - `X11Unavailable` → triggers `NoBackendAvailable` if portal also failed
/// - `NoBackendAvailable` → `Err(message)` with descriptive string
/// - `CaptureStartFailed` → `Err(message)` with backend-specific detail
#[derive(Debug)]
pub enum CaptureError {
    /// Portal picker was dismissed by the user — maps to Ok(false).
    #[allow(dead_code)]
    UserCancelled,
    /// D-Bus call to xdg-desktop-portal failed; will try X11 fallback.
    #[allow(dead_code)]
    PortalUnavailable(String),
    /// No DISPLAY env var / XWayland not running — X11 capture impossible.
    #[allow(dead_code)]
    X11Unavailable(String),
    /// Neither portal nor X11 capture is available. On pure Wayland without
    /// xdg-desktop-portal and without XWayland the message explicitly states:
    /// "screen sharing requires xdg-desktop-portal on Wayland; no X11 fallback
    /// is available on this system".
    #[allow(dead_code)]
    NoBackendAvailable(String),
    /// A backend-specific failure after capture was attempted.
    CaptureStartFailed(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::UserCancelled => write!(f, "screen share cancelled by user"),
            CaptureError::PortalUnavailable(msg) => {
                write!(f, "xdg-desktop-portal unavailable: {msg}")
            }
            CaptureError::X11Unavailable(msg) => write!(f, "X11 capture unavailable: {msg}"),
            CaptureError::NoBackendAvailable(msg) => {
                write!(f, "no capture backend available: {msg}")
            }
            CaptureError::CaptureStartFailed(msg) => {
                write!(f, "capture start failed: {msg}")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

#[cfg(target_os = "linux")]
static PIPEWIRE_INIT: OnceLock<()> = OnceLock::new();

#[cfg(target_os = "linux")]
pub fn ensure_pipewire_init() {
    PIPEWIRE_INIT.get_or_init(|| {
        // Explicit pipewire::init() is crashing on some Wayland/Hyprland systems
        // before control returns. Rely on the subsequent PipeWire object creation
        // path instead of eagerly initializing the library here.
        crate::debug_eprintln!("wavis: screen_capture: pipewire init skipped");
    });
}

/// A single captured frame of RGBA pixel data.
pub struct CapturedFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Raw RGBA pixel data (4 bytes per pixel, row-major).
    pub data: Vec<u8>,
    /// Capture timestamp in milliseconds since an arbitrary epoch.
    pub timestamp_ms: u64,
}

/// Platform-abstracted screen capture interface.
///
/// Implementations handle the platform-specific capture pipeline (PipeWire/portal
/// or X11/XShm) and deliver frames via the registered callback.
pub trait ScreenCapture: Send + Sync {
    /// Start capturing. Returns `Ok(())` on success.
    ///
    /// # Errors
    /// - `CaptureError::UserCancelled` — portal picker dismissed
    /// - `CaptureError::PortalUnavailable` — D-Bus call failed
    /// - `CaptureError::X11Unavailable` — no DISPLAY / XWayland
    /// - `CaptureError::NoBackendAvailable` — neither backend works
    /// - `CaptureError::CaptureStartFailed` — backend-specific failure
    #[allow(dead_code)]
    fn start(&self) -> Result<(), CaptureError>;

    /// Stop capturing and release resources.
    fn stop(&self);

    /// Returns `true` if a capture session is currently active.
    #[allow(dead_code)]
    fn is_active(&self) -> bool;

    /// Register a callback to receive captured frames.
    ///
    /// The callback is invoked on a background thread for each captured frame.
    /// Only one callback is active at a time — calling this again replaces the
    /// previous callback.
    fn on_frame(&self, cb: Box<dyn Fn(CapturedFrame) + Send + 'static>);
}

/// Create and start a screen capture backend using the fallback chain:
///
/// 1. Try PipeWire/portal (primary) → if `PortalUnavailable`, continue
/// 2. Try X11/XWayland (fallback) → if `X11Unavailable`, continue
/// 3. Return `NoBackendAvailable` with a descriptive message
///
/// Returns a started `Box<dyn ScreenCapture>` on success. The caller owns the
/// lifecycle — call `stop()` when done.
///
/// # Error mapping at the IPC layer
/// - `UserCancelled` → `Ok(false)` (user dismissed the portal picker)
/// - `NoBackendAvailable(msg)` / `CaptureStartFailed(msg)` → `Err(msg)`
#[cfg(target_os = "linux")]
pub fn create_capture_backend(
    max_width: u32,
    max_height: u32,
    max_fps: u32,
) -> Result<Box<dyn ScreenCapture>, CaptureError> {
    // Step 1: Try PipeWire/portal (primary path).
    log::info!("screen_capture: trying PipeWire/portal backend");
    let pw = pipewire_capture::PipeWireCapture::new(max_width, max_height, max_fps);
    match pw.start() {
        Ok(()) => return Ok(Box::new(pw)),
        Err(CaptureError::UserCancelled) => return Err(CaptureError::UserCancelled),
        Err(CaptureError::PortalUnavailable(msg)) => {
            log::info!("Portal unavailable ({msg}), trying X11 fallback");
        }
        Err(e) => return Err(e),
    }

    // Step 2: Try X11/XWayland fallback.
    log::info!("screen_capture: trying X11 fallback backend");
    let x11 = x11_capture::X11Capture::new();
    match x11.start() {
        Ok(()) => Ok(Box::new(x11)),
        Err(CaptureError::X11Unavailable(msg)) => {
            // On pure Wayland without XWayland, provide a specific message
            // so the user understands the environment gap.
            if std::env::var("WAYLAND_DISPLAY").is_ok() {
                Err(CaptureError::NoBackendAvailable(
                    "screen sharing requires xdg-desktop-portal on Wayland; \
                     no X11 fallback is available on this system"
                        .into(),
                ))
            } else {
                Err(CaptureError::NoBackendAvailable(msg))
            }
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `CaptureError::NoBackendAvailable` Display output includes
    /// the inner message.
    #[test]
    fn capture_error_display_no_backend() {
        let err = CaptureError::NoBackendAvailable("test message".into());
        assert!(err.to_string().contains("test message"));
    }

    /// Verify that `CaptureError::UserCancelled` Display output is descriptive.
    #[test]
    fn capture_error_display_user_cancelled() {
        let err = CaptureError::UserCancelled;
        assert_eq!(err.to_string(), "screen share cancelled by user");
    }

    /// Verify the Wayland-specific error message content.
    #[test]
    fn wayland_error_message_mentions_portal() {
        let msg = "screen sharing requires xdg-desktop-portal on Wayland; \
                   no X11 fallback is available on this system";
        let err = CaptureError::NoBackendAvailable(msg.into());
        let display = err.to_string();
        assert!(display.contains("xdg-desktop-portal"));
        assert!(display.contains("Wayland"));
    }
}
