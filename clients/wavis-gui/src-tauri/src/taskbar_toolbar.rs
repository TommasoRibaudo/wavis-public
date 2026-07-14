//! Windows thumbnail toolbar — mute / deafen / leave buttons shown in the
//! taskbar preview when the app is minimized.
//!
//! Buttons are currently hidden (ThumbBarAddButtons not called) until custom
//! icons are ready. The window subclass and state listener are kept so wiring
//! up buttons later requires only adding ThumbBarAddButtons + apply_button_state.
//!
//! Reuses `tray-event` and `tray-state-update` events — no frontend changes needed.

use std::sync::{
    atomic::{AtomicIsize, Ordering},
    Mutex,
};

use serde::Deserialize;
use tauri::{App, Emitter, Listener, Manager};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        CallWindowProcW, PostMessageW, SetWindowLongPtrW, GWLP_WNDPROC, WM_COMMAND, WNDPROC,
    },
};

// ── Constants ──────────────────────────────────────────────────────────────

const BTN_MUTE: u32 = 201;
const BTN_DEAFEN: u32 = 202;
const BTN_LEAVE: u32 = 203;

// WM_APP + 1 — reserved for future button-state refresh messages.
const WM_TASKBAR_UPDATE: u32 = 0x8001;

// ── State ──────────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Clone, Default)]
struct TaskbarVoiceState {
    in_voice_session: bool,
    is_muted: bool,
    is_deafened: bool,
}

struct TaskbarGlobal {
    hwnd_raw: isize,
    app_handle: tauri::AppHandle,
    state: TaskbarVoiceState,
}

static PREV_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static GLOBAL: Mutex<Option<TaskbarGlobal>> = Mutex::new(None);

// ── Window subclass ────────────────────────────────────────────────────────

#[derive(Clone, serde::Serialize)]
struct TrayEventPayload {
    action: &'static str,
}

// SAFETY: installed as the window procedure via `SetWindowLongPtrW` below, so
// Windows calls this only on the UI thread with a valid `hwnd` for this
// window. `PREV_WNDPROC` is only ever populated with the `WNDPROC` returned
// by that same `SetWindowLongPtrW` call, so the `transmute` back to
// `WNDPROC` reconstructs a value of the type it was stored from.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u32;
            let action: Option<&'static str> = match id {
                BTN_MUTE => Some("toggle-mute"),
                BTN_DEAFEN => Some("toggle-deafen"),
                BTN_LEAVE => Some("leave"),
                _ => None,
            };
            if let Some(action) = action {
                if let Ok(guard) = GLOBAL.lock() {
                    if let Some(g) = guard.as_ref() {
                        let _ = g.app_handle.emit("tray-event", TrayEventPayload { action });
                    }
                }
                return LRESULT(0);
            }
        }
        // Reserved for future button-state updates via PostMessageW.
        WM_TASKBAR_UPDATE => return LRESULT(0),
        _ => {}
    }
    let prev: WNDPROC = std::mem::transmute(PREV_WNDPROC.load(Ordering::SeqCst));
    CallWindowProcW(prev, hwnd, msg, wparam, lparam)
}

// ── Payload (mirrors tray.rs) ──────────────────────────────────────────────

#[derive(Deserialize)]
struct TrayStatePayload {
    #[serde(rename = "inVoiceSession")]
    in_voice_session: bool,
    #[serde(rename = "isMuted")]
    is_muted: bool,
    #[serde(rename = "isDeafened")]
    is_deafened: bool,
}

// ── Public setup ───────────────────────────────────────────────────────────

pub fn setup_taskbar_toolbar(app: &App) -> Result<(), Box<dyn std::error::Error>> {
    let window = app
        .get_webview_window("main")
        .ok_or("taskbar_toolbar: main window not found")?;

    let raw = window
        .hwnd()
        .map_err(|e| format!("taskbar_toolbar: hwnd: {e}"))?;
    let hwnd = HWND(raw.0);

    *GLOBAL.lock().unwrap() = Some(TaskbarGlobal {
        hwnd_raw: hwnd.0 as isize,
        app_handle: app.handle().clone(),
        state: TaskbarVoiceState::default(),
    });

    // SAFETY: `hwnd` was just obtained from `window.hwnd()` on the still-live
    // main window, so it's a valid handle on this thread. `wndproc` has the
    // `unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT`
    // signature `SetWindowLongPtrW` requires for `GWLP_WNDPROC`, and it stays
    // valid for the app's lifetime (it's a `static fn`, no captures to drop).
    unsafe {
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wndproc as *const () as isize);
        if prev == 0 {
            log::warn!("taskbar_toolbar: SetWindowLongPtrW returned 0");
        }
        PREV_WNDPROC.store(prev, Ordering::SeqCst);

        // ThumbBarAddButtons not called — buttons hidden until custom icons are ready.
    }

    // Track voice state so button state is correct the moment icons are wired up.
    app.listen("tray-state-update", |event| {
        let Ok(payload) = serde_json::from_str::<TrayStatePayload>(event.payload()) else {
            return;
        };
        let new_state = TaskbarVoiceState {
            in_voice_session: payload.in_voice_session,
            is_muted: payload.is_muted,
            is_deafened: payload.is_deafened,
        };
        let hwnd_raw = {
            let Ok(mut guard) = GLOBAL.lock() else { return };
            let Some(g) = guard.as_mut() else { return };
            g.state = new_state;
            g.hwnd_raw
        };
        // SAFETY: `PostMessageW` only enqueues a message on the window's
        // message queue — it doesn't dereference `hwnd_raw`. `hwnd_raw` was
        // captured from the same valid `HWND` used to install `wndproc`
        // above, and a stale/closed handle just makes the call return an
        // ignorable error (`let _ =`), not a safety violation.
        unsafe {
            let _ = PostMessageW(
                HWND(hwnd_raw as *mut _),
                WM_TASKBAR_UPDATE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });

    Ok(())
}
