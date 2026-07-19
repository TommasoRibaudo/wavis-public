//! Mitigation for a WebView2-on-Windows compositor paint-stall: chromeless
//! (`decorations:false`) windows occasionally stop repainting while sitting
//! open, and only recover when the user manually resizes the window (#329).
//! Every Wavis window is chromeless — the main window (`tauri.conf.json`) and
//! the four child window types built from `CHILD_WINDOW_BASE_OPTIONS` in
//! `share-window-bridge.ts` — so all are equally exposed.
//!
//! The mitigation is a trivial resize: growing a window by one physical
//! pixel and immediately restoring it forces WebView2 to recreate its
//! swap-chain surface and redraw, with no user-visible size change. It's
//! applied on two triggers: a window regaining focus, and a periodic timer
//! for windows that stay open and idle without ever being refocused (the
//! reported case — the window went blank while nobody was interacting with
//! it).
//!
//! Windows-only: this compositor bug has not been reported on macOS/Linux,
//! and the fix is scoped to Windows only (#329).

use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalSize, Size};

/// How often the periodic defensive nudge sweeps visible windows.
const PERIODIC_NUDGE_INTERVAL: Duration = Duration::from_secs(180);

/// Window labels affected by the chromeless-window paint-stall. Must stay in
/// sync with every window label created via `WebviewWindowBuilder`/`new
/// WebviewWindow` — see `share-window-bridge.ts`'s child window constructors
/// and `tauri.conf.json`'s main window entry.
pub fn is_repaint_nudge_target(label: &str) -> bool {
    matches!(
        label,
        "main" | "watch-all" | "camera-popout" | "share-picker"
    ) || label.starts_with("screen-share-")
}

/// Whether a periodic sweep should nudge this window: in scope, currently
/// visible, and not minimized. A destroyed window never reaches this check —
/// it's simply absent from `AppHandle::webview_windows()`.
pub fn should_periodic_nudge(label: &str, visible: bool, minimized: bool) -> bool {
    is_repaint_nudge_target(label) && visible && !minimized
}

/// Force a WebView2/compositor redraw without a visible size change: grow
/// the window by one physical pixel, then immediately restore its original
/// size.
pub fn nudge_repaint(window: &tauri::WebviewWindow) {
    let Ok(size) = window.inner_size() else {
        return;
    };
    let nudged = PhysicalSize::new(size.width.saturating_add(1), size.height);
    let _ = window.set_size(Size::Physical(nudged));
    let _ = window.set_size(Size::Physical(size));
}

/// Start the periodic defensive nudge: every `PERIODIC_NUDGE_INTERVAL`,
/// sweep all open windows and nudge the ones that are in scope, visible, and
/// not minimized. Runs for the lifetime of the app process.
pub fn start_periodic_nudge(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(PERIODIC_NUDGE_INTERVAL);
        ticker.tick().await; // first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            for (label, window) in app.webview_windows() {
                let visible = window.is_visible().unwrap_or(false);
                let minimized = window.is_minimized().unwrap_or(false);
                if should_periodic_nudge(&label, visible, minimized) {
                    nudge_repaint(&window);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_window_is_a_nudge_target() {
        assert!(is_repaint_nudge_target("main"));
    }

    #[test]
    fn watch_all_is_a_nudge_target() {
        assert!(is_repaint_nudge_target("watch-all"));
    }

    #[test]
    fn camera_popout_is_a_nudge_target() {
        assert!(is_repaint_nudge_target("camera-popout"));
    }

    #[test]
    fn share_picker_is_a_nudge_target() {
        assert!(is_repaint_nudge_target("share-picker"));
    }

    #[test]
    fn screen_share_popouts_are_nudge_targets_regardless_of_participant_id() {
        assert!(is_repaint_nudge_target("screen-share-618368a7-d0ab-43ce"));
        assert!(is_repaint_nudge_target("screen-share-"));
    }

    #[test]
    fn unknown_labels_are_not_nudge_targets() {
        assert!(!is_repaint_nudge_target("diagnostics"));
        assert!(!is_repaint_nudge_target(""));
        assert!(!is_repaint_nudge_target("screen-sharing"));
    }

    #[test]
    fn periodic_nudge_skips_hidden_windows() {
        assert!(!should_periodic_nudge("watch-all", false, false));
    }

    #[test]
    fn periodic_nudge_skips_minimized_windows() {
        assert!(!should_periodic_nudge("watch-all", true, true));
    }

    #[test]
    fn periodic_nudge_skips_out_of_scope_windows() {
        assert!(!should_periodic_nudge("diagnostics", true, false));
    }

    #[test]
    fn periodic_nudge_fires_for_visible_non_minimized_in_scope_windows() {
        assert!(should_periodic_nudge("watch-all", true, false));
        assert!(should_periodic_nudge("main", true, false));
    }
}
