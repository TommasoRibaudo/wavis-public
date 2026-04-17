//! Wayland capture authorization via XDG Desktop Portal.
//!
//! On Wayland, direct PipeWire node access (used by `list_share_sources` and
//! `screen_share_start_source`) bypasses the XDG Desktop Portal, which is the
//! standard mechanism for granting screen capture permission. Without portal
//! authorization, PipeWire may refuse to enumerate or capture video nodes.
//!
//! This module acquires a single portal `ScreenCast` session at first use to
//! obtain a PipeWire fd with capture permission. The portal dialog opens once,
//! the user grants access, and the returned PipeWire fd is reused for all
//! subsequent enumeration and capture operations.
//!
//! On X11, direct PipeWire access does not require portal authorization —
//! the module detects this and skips the portal entirely.

use std::os::fd::OwnedFd;
use std::sync::Mutex;

/// Display server type detected at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServer {
    /// Wayland compositor — portal authorization required for PipeWire access.
    Wayland,
    /// X11 display server — direct PipeWire access works without portal.
    X11,
    /// Unknown — treat as X11 (best-effort, no portal).
    Unknown,
}

/// Result of a portal authorization attempt.
#[derive(Debug)]
pub enum AuthResult {
    /// Portal session acquired — fd is valid for PipeWire operations.
    Authorized,
    /// Portal was denied by the user or unavailable — on Wayland this means
    /// we must fall back to the portal-based native picker (degraded UX).
    Denied(String),
    /// Not needed — running on X11 where direct PipeWire access works.
    NotNeeded,
}

/// Holds the authorized PipeWire fd from a portal ScreenCast session.
///
/// Managed as Tauri state and shared across `list_share_sources`,
/// `fetch_source_thumbnail`, and `screen_share_start_source`.
pub struct PortalAuthState {
    /// The authorized PipeWire fd, if a portal session has been acquired.
    /// `None` means either: (a) not yet authorized, or (b) running on X11.
    inner: Mutex<PortalAuthInner>,
}

struct PortalAuthInner {
    /// Authorized PipeWire fd from the portal session.
    pw_fd: Option<OwnedFd>,
    /// Detected display server.
    display_server: DisplayServer,
    /// Whether authorization has been attempted (to avoid repeated portal dialogs).
    attempted: bool,
}

impl PortalAuthState {
    /// Create a new `PortalAuthState` with no authorization.
    pub fn new() -> Self {
        let display_server = detect_display_server();
        log::info!(
            "[wavis:portal-auth] detected display server: {:?}",
            display_server
        );
        Self {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: None,
                display_server,
                attempted: false,
            }),
        }
    }

    /// Returns the detected display server type.
    pub fn display_server(&self) -> DisplayServer {
        self.inner
            .lock()
            .map(|g| g.display_server)
            .unwrap_or(DisplayServer::Unknown)
    }

    /// Returns `true` if an authorized PipeWire fd is available.
    pub fn is_authorized(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.pw_fd.is_some())
            .unwrap_or(false)
    }

    /// Returns `true` if authorization has already been attempted (success or failure).
    pub fn was_attempted(&self) -> bool {
        self.inner.lock().map(|g| g.attempted).unwrap_or(false)
    }

    /// Returns `true` if portal authorization is needed (Wayland and not yet authorized).
    pub fn needs_auth(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.display_server == DisplayServer::Wayland && g.pw_fd.is_none())
            .unwrap_or(false)
    }

    /// Borrow the authorized PipeWire fd for use in PipeWire operations.
    ///
    /// Returns `None` if not authorized or running on X11 (where it's not needed).
    /// The fd remains owned by this struct — callers must use `BorrowedFd` or
    /// duplicate it via `OwnedFd::try_clone()` if they need a separate handle.
    pub fn try_clone_fd(&self) -> Option<OwnedFd> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.pw_fd.as_ref().and_then(|fd| fd.try_clone().ok()))
    }

    /// Attempt to acquire portal authorization.
    ///
    /// On X11: returns `AuthResult::NotNeeded` immediately.
    /// On Wayland: opens the portal ScreenCast dialog once. If the user grants
    /// access, stores the PipeWire fd for reuse. If denied or unavailable,
    /// returns `AuthResult::Denied` with a reason string.
    ///
    /// This is idempotent — if already authorized, returns `Authorized` without
    /// opening a new dialog. If previously denied, returns the cached denial.
    pub fn authorize(&self) -> AuthResult {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return AuthResult::Denied("internal lock error".to_string()),
        };

        // X11 doesn't need portal authorization.
        if inner.display_server != DisplayServer::Wayland {
            return AuthResult::NotNeeded;
        }

        // Already authorized — reuse existing fd.
        if inner.pw_fd.is_some() {
            return AuthResult::Authorized;
        }

        // Already attempted and failed — don't show the dialog again.
        if inner.attempted {
            return AuthResult::Denied(
                "portal authorization was previously denied or unavailable".to_string(),
            );
        }

        inner.attempted = true;

        // Acquire the portal session. This opens the native portal dialog.
        log::info!("[wavis:portal-auth] requesting portal ScreenCast authorization");
        match acquire_portal_fd() {
            Ok(fd) => {
                log::info!("[wavis:portal-auth] portal authorization granted");
                inner.pw_fd = Some(fd);
                AuthResult::Authorized
            }
            Err(reason) => {
                log::warn!("[wavis:portal-auth] portal authorization failed: {reason}");
                AuthResult::Denied(reason)
            }
        }
    }
}

/// Detect whether we're running on Wayland or X11.
fn detect_display_server() -> DisplayServer {
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        DisplayServer::Wayland
    } else if std::env::var("DISPLAY").is_ok() {
        DisplayServer::X11
    } else {
        DisplayServer::Unknown
    }
}

/// Acquire a PipeWire fd from the XDG Desktop Portal ScreenCast interface.
///
/// Opens a portal session with `Multiple` source selection (monitors + windows),
/// starts it (which may show a native dialog), and returns the PipeWire remote fd.
/// The fd grants permission for direct PipeWire node access on Wayland.
fn acquire_portal_fd() -> Result<OwnedFd, String> {
    // We need a tokio runtime for the async ashpd calls.
    // Use the current runtime if available, otherwise create a temporary one.
    let rt = tokio::runtime::Handle::try_current()
        .map(TempRuntime::Existing)
        .unwrap_or_else(|_| {
            TempRuntime::Owned(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create temp runtime for portal auth"),
            )
        });

    rt.block_on(acquire_portal_fd_async())
}

enum TempRuntime {
    Existing(tokio::runtime::Handle),
    Owned(tokio::runtime::Runtime),
}

impl TempRuntime {
    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        match self {
            TempRuntime::Existing(h) => {
                // We're already on a runtime — use block_in_place to avoid nesting.
                tokio::task::block_in_place(|| h.block_on(f))
            }
            TempRuntime::Owned(rt) => rt.block_on(f),
        }
    }
}

/// Async inner implementation of portal fd acquisition.
async fn acquire_portal_fd_async() -> Result<OwnedFd, String> {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::PersistMode;

    let proxy = Screencast::new()
        .await
        .map_err(|e| format!("xdg-desktop-portal ScreenCast unavailable: {e}"))?;

    let session = proxy
        .create_session()
        .await
        .map_err(|e| format!("failed to create portal session: {e}"))?;

    // Select all source types (monitors + windows) so the fd grants broad access.
    proxy
        .select_sources(
            &session,
            CursorMode::Hidden,
            SourceType::Monitor | SourceType::Window,
            true, // multiple sources
            None, // no restore token
            PersistMode::DoNot,
        )
        .await
        .map_err(|e| format!("failed to select sources: {e}"))?;

    // Start the session — this may show the native portal dialog.
    let _response = proxy
        .start(&session, None)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("cancelled") || msg.contains("Cancelled") {
                "user cancelled the portal dialog".to_string()
            } else {
                format!("portal start failed: {e}")
            }
        })?
        .response()
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("cancelled") || msg.contains("Cancelled") {
                "user cancelled the portal dialog".to_string()
            } else {
                format!("portal response failed: {e}")
            }
        })?;

    // Get the PipeWire remote fd — this is the authorized fd we'll reuse.
    let pw_fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(|e| format!("failed to open PipeWire remote: {e}"))?;

    Ok(pw_fd)
}

/// Tauri command: request portal authorization for screen capture on Wayland.
///
/// Returns a JSON-serializable result indicating the authorization outcome.
/// The frontend should call this once (e.g., on first share attempt) and
/// handle the result:
/// - `"authorized"` — portal granted access, direct PipeWire operations will work
/// - `"not_needed"` — running on X11, no portal needed
/// - `"denied:{reason}"` — portal denied or unavailable, fall back to native picker
#[tauri::command]
pub fn authorize_screen_capture(
    state: tauri::State<'_, PortalAuthState>,
) -> Result<String, String> {
    match state.authorize() {
        AuthResult::Authorized => Ok("authorized".to_string()),
        AuthResult::NotNeeded => Ok("not_needed".to_string()),
        AuthResult::Denied(reason) => Ok(format!("denied:{reason}")),
    }
}

/// Tauri command: check if portal authorization is available/needed.
///
/// Returns a JSON object with the current authorization status.
#[tauri::command]
pub fn get_capture_auth_status(
    state: tauri::State<'_, PortalAuthState>,
) -> Result<CaptureAuthStatus, String> {
    Ok(CaptureAuthStatus {
        display_server: match state.display_server() {
            DisplayServer::Wayland => "wayland".to_string(),
            DisplayServer::X11 => "x11".to_string(),
            DisplayServer::Unknown => "unknown".to_string(),
        },
        authorized: state.is_authorized(),
        needs_auth: state.needs_auth(),
        was_attempted: state.was_attempted(),
    })
}

/// Authorization status returned to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureAuthStatus {
    /// Detected display server: "wayland", "x11", or "unknown".
    pub display_server: String,
    /// Whether an authorized PipeWire fd is available.
    pub authorized: bool,
    /// Whether portal authorization is needed (Wayland + not yet authorized).
    pub needs_auth: bool,
    /// Whether authorization has been attempted.
    pub was_attempted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_server_detection_returns_valid_variant() {
        let ds = detect_display_server();
        assert!(matches!(
            ds,
            DisplayServer::Wayland | DisplayServer::X11 | DisplayServer::Unknown
        ));
    }

    #[test]
    fn portal_auth_state_new_not_authorized() {
        let state = PortalAuthState::new();
        assert!(!state.is_authorized());
        assert!(!state.was_attempted());
    }

    #[test]
    fn portal_auth_state_try_clone_fd_none_when_not_authorized() {
        let state = PortalAuthState::new();
        assert!(state.try_clone_fd().is_none());
    }

    #[test]
    fn portal_auth_x11_returns_not_needed() {
        // Force X11 detection by constructing inner directly.
        let state = PortalAuthState {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: None,
                display_server: DisplayServer::X11,
                attempted: false,
            }),
        };
        match state.authorize() {
            AuthResult::NotNeeded => {} // expected
            other => panic!("expected NotNeeded, got {:?}", other),
        }
        assert!(!state.is_authorized());
    }

    #[test]
    fn portal_auth_unknown_returns_not_needed() {
        let state = PortalAuthState {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: None,
                display_server: DisplayServer::Unknown,
                attempted: false,
            }),
        };
        match state.authorize() {
            AuthResult::NotNeeded => {}
            other => panic!("expected NotNeeded, got {:?}", other),
        }
    }

    #[test]
    fn portal_auth_already_attempted_returns_denied() {
        let state = PortalAuthState {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: None,
                display_server: DisplayServer::Wayland,
                attempted: true,
            }),
        };
        match state.authorize() {
            AuthResult::Denied(msg) => {
                assert!(msg.contains("previously denied"));
            }
            other => panic!("expected Denied, got {:?}", other),
        }
    }

    #[test]
    fn portal_auth_already_authorized_returns_authorized() {
        // Create a dummy fd using a Unix socket pair.
        let (read_fd, _write_fd) = nix_pipe();
        let state = PortalAuthState {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: Some(read_fd),
                display_server: DisplayServer::Wayland,
                attempted: true,
            }),
        };
        match state.authorize() {
            AuthResult::Authorized => {}
            other => panic!("expected Authorized, got {:?}", other),
        }
        assert!(state.is_authorized());
        assert!(state.try_clone_fd().is_some());
    }

    /// Helper to create a pair of fds for testing fd storage.
    fn nix_pipe() -> (OwnedFd, OwnedFd) {
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair().expect("failed to create socket pair");
        (OwnedFd::from(a), OwnedFd::from(b))
    }

    #[test]
    fn needs_auth_true_on_wayland_without_fd() {
        let state = PortalAuthState {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: None,
                display_server: DisplayServer::Wayland,
                attempted: false,
            }),
        };
        assert!(state.needs_auth());
    }

    #[test]
    fn needs_auth_false_on_x11() {
        let state = PortalAuthState {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: None,
                display_server: DisplayServer::X11,
                attempted: false,
            }),
        };
        assert!(!state.needs_auth());
    }

    #[test]
    fn needs_auth_false_when_authorized() {
        let (read_fd, _write_fd) = nix_pipe();
        let state = PortalAuthState {
            inner: Mutex::new(PortalAuthInner {
                pw_fd: Some(read_fd),
                display_server: DisplayServer::Wayland,
                attempted: true,
            }),
        };
        assert!(!state.needs_auth());
    }
}
