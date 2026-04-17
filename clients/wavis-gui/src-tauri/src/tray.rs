use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Emitter, Listener, Manager,
};

/* ─── Managed State ─────────────────────────────────────────────── */

/// Synchronous flag for the close event handler.
/// Updated by the frontend via the `minimize-to-tray-changed` event.
#[derive(Clone)]
pub struct MinimizeToTrayFlag {
    pub enabled: Arc<AtomicBool>,
}

/// Tracks whether the main window is currently hidden (minimized to tray).
#[derive(Clone)]
pub struct WindowVisibility {
    pub hidden: Arc<AtomicBool>,
}

#[derive(Deserialize)]
struct MinimizeToTrayPayload {
    enabled: bool,
}

#[derive(Clone, Serialize)]
struct WindowVisibilityPayload {
    visible: bool,
}

pub fn setup_tray(app: &App) -> Result<(), Box<dyn std::error::Error>> {
    // ── Managed state: minimize-to-tray flag (synchronous read in close handler) ──
    let minimize_flag = MinimizeToTrayFlag {
        enabled: Arc::new(AtomicBool::new(false)),
    };
    app.manage(minimize_flag.clone());

    // ── Managed state: window visibility tracking (R19.7) ──
    let window_visibility = WindowVisibility {
        hidden: Arc::new(AtomicBool::new(false)),
    };
    app.manage(window_visibility.clone());

    // ── Listen for minimize-to-tray-changed from frontend ──
    let flag = minimize_flag.clone();
    app.listen("minimize-to-tray-changed", move |event| {
        if let Ok(payload) = serde_json::from_str::<MinimizeToTrayPayload>(event.payload()) {
            flag.enabled.store(payload.enabled, Ordering::SeqCst);
        }
    });

    // ── Tray menu ──
    let show = MenuItem::with_id(app, "show", "Show Wavis", true, None::<&str>)?;
    let mute = MenuItem::with_id(app, "mute", "Toggle Mute", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show, &mute, &quit])?;

    let _tray = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                if let Some(vis) = app.try_state::<WindowVisibility>() {
                    vis.hidden.store(false, Ordering::SeqCst);
                }
                let _ = app.emit(
                    "window-visibility-changed",
                    WindowVisibilityPayload { visible: true },
                );
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

/// Handle the window close event. If minimize-to-tray is enabled, hide the
/// window instead of closing it. Returns `true` if the close was intercepted.
pub fn handle_close_requested(
    window: &tauri::WebviewWindow,
    minimize_flag: &MinimizeToTrayFlag,
    visibility: &WindowVisibility,
) -> bool {
    if minimize_flag.enabled.load(Ordering::SeqCst) {
        let _ = window.hide();
        visibility.hidden.store(true, Ordering::SeqCst);
        let _ = window.emit(
            "window-visibility-changed",
            WindowVisibilityPayload { visible: false },
        );
        true
    } else {
        false
    }
}
