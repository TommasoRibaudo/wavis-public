use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
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

#[derive(Deserialize)]
struct TrayStatePayload {
    #[serde(rename = "inVoiceSession")]
    in_voice_session: bool,
    #[serde(rename = "isMuted")]
    is_muted: bool,
    #[serde(rename = "isDeafened")]
    is_deafened: bool,
}

#[derive(Clone, Serialize)]
struct TrayEventPayload {
    action: &'static str,
}

#[derive(Clone, Serialize)]
pub struct WindowVisibilityPayload {
    pub visible: bool,
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
    let mute = MenuItem::with_id(app, "mute", "Mute", false, None::<&str>)?;
    let deafen = MenuItem::with_id(app, "deafen", "Deafen", false, None::<&str>)?;
    let leave = MenuItem::with_id(app, "leave", "Leave Room", false, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu_items: [&dyn IsMenuItem<_>; 6] = [&show, &separator, &mute, &deafen, &leave, &quit];
    let menu = Menu::with_items(app, &menu_items)?;

    let mute_state_item = mute.clone();
    let deafen_state_item = deafen.clone();
    let leave_state_item = leave.clone();
    app.listen("tray-state-update", move |event| {
        if let Ok(payload) = serde_json::from_str::<TrayStatePayload>(event.payload()) {
            let _ = mute_state_item.set_text(if payload.is_muted { "Unmute" } else { "Mute" });
            let _ = deafen_state_item
                .set_text(if payload.is_deafened { "Undeafen" } else { "Deafen" });
            let _ = mute_state_item.set_enabled(payload.in_voice_session);
            let _ = deafen_state_item.set_enabled(payload.in_voice_session);
            let _ = leave_state_item.set_enabled(payload.in_voice_session);
        }
    });

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .icon_as_template(true)
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
            "mute" => {
                let _ = app.emit(
                    "tray-event",
                    TrayEventPayload {
                        action: "toggle-mute",
                    },
                );
            }
            "deafen" => {
                let _ = app.emit(
                    "tray-event",
                    TrayEventPayload {
                        action: "toggle-deafen",
                    },
                );
            }
            "leave" => {
                let _ = app.emit("tray-event", TrayEventPayload { action: "leave" });
            }
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let _tray = builder.build(app)?;

    // Global handler — fires for every tray icon event with no per-icon ID
    // matching, making it reliable across all Tauri runtime versions.
    app.on_tray_icon_event(|app, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
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
    });

    Ok(())
}

/// Show and focus the main window from any state (tray-hidden, minimized, or normal).
/// Clears the hidden flag and emits `window-visibility-changed` so the frontend
/// tracks tray state correctly regardless of how the window was restored.
#[tauri::command]
pub fn show_main_window(
    app: tauri::AppHandle,
    visibility: tauri::State<'_, WindowVisibility>,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    visibility.hidden.store(false, Ordering::SeqCst);
    let _ = app.emit("window-visibility-changed", WindowVisibilityPayload { visible: true });
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
