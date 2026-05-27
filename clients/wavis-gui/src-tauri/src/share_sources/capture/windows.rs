//! Win32 and Graphics Capture share-source enumeration and thumbnail capture (Windows).
//!
//! This module owns Win32 and Graphics Capture integration for screen, window,
//! and per-process audio source discovery. Shared DTOs, thumbnail encoding, and
//! Tauri command wiring remain in the parent `share_sources` module.

use super::super::{
    compute_fallback_reason, encode_thumbnail_jpeg, should_include_window, EnumerationResult,
    FallbackReason, ShareSource, ShareSourceType, WindowDescriptor, THUMBNAIL_TIMEOUT,
};

/// Windows enumeration timeout. Source discovery should return quickly so the
/// picker stays responsive even if one Win32 call stalls.
const WIN_ENUM_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

fn measure_window_area(hwnd: windows::Win32::Foundation::HWND) -> u32 {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};

    unsafe {
        let mut client_rect = RECT::default();
        if GetClientRect(hwnd, &mut client_rect).is_ok() {
            let width = (client_rect.right - client_rect.left).max(0) as u32;
            let height = (client_rect.bottom - client_rect.top).max(0) as u32;
            let client_area = width.saturating_mul(height);
            if client_area > 0 {
                return client_area;
            }
        }

        // Some Chromium-family windows intermittently report a zero client rect
        // during enumeration even though the top-level window is visible.
        let mut window_rect = RECT::default();
        if GetWindowRect(hwnd, &mut window_rect).is_ok() {
            let width = (window_rect.right - window_rect.left).max(0) as u32;
            let height = (window_rect.bottom - window_rect.top).max(0) as u32;
            return width.saturating_mul(height);
        }
    }

    0
}

/// Check whether the Windows Graphics Capture API is available.
fn check_graphics_capture_available() -> bool {
    use windows::Graphics::Capture::GraphicsCaptureSession;

    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

/// Enumerate monitors via Win32 `EnumDisplayMonitors` + `GetMonitorInfoW`.
fn enumerate_monitors(deadline: std::time::Instant) -> Result<Vec<ShareSource>, String> {
    use std::mem;
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, MONITORINFOEXW,
    };

    struct EnumState {
        sources: Vec<ShareSource>,
        deadline: std::time::Instant,
    }

    unsafe extern "system" fn monitor_callback(
        hmonitor: windows::Win32::Graphics::Gdi::HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let state = &mut *(lparam.0 as *mut EnumState);

        if std::time::Instant::now() > state.deadline {
            return BOOL(0);
        }

        let mut info: MONITORINFOEXW = mem::zeroed();
        info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;

        let got_info = GetMonitorInfoW(hmonitor, &mut info as *mut MONITORINFOEXW as *mut _);

        let name = if got_info.as_bool() {
            let device = &info.szDevice;
            let len = device.iter().position(|&c| c == 0).unwrap_or(device.len());
            let display_name = String::from_utf16_lossy(&device[..len]);
            if display_name.is_empty() {
                format!("Display {}", state.sources.len() + 1)
            } else {
                display_name
                    .rsplit('\\')
                    .next()
                    .unwrap_or(&display_name)
                    .strip_prefix("DISPLAY")
                    .map(|n| format!("Display {n}"))
                    .unwrap_or_else(|| format!("Display {}", state.sources.len() + 1))
            }
        } else {
            format!("Display {}", state.sources.len() + 1)
        };

        state.sources.push(ShareSource {
            id: format!("{}", hmonitor.0 as isize),
            name,
            source_type: ShareSourceType::Screen,
            thumbnail: None,
            app_name: None,
        });

        BOOL(1)
    }

    let mut state = EnumState {
        sources: Vec::new(),
        deadline,
    };

    let success = unsafe {
        EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_callback),
            LPARAM(&mut state as *mut EnumState as isize),
        )
    };

    if !success.as_bool() && state.sources.is_empty() {
        return Err("EnumDisplayMonitors failed".to_string());
    }

    Ok(state.sources)
}

/// Enumerate visible top-level windows via `EnumWindows`.
fn enumerate_windows(deadline: std::time::Instant) -> Result<Vec<ShareSource>, String> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    };

    let self_pid = std::process::id();

    struct EnumState {
        sources: Vec<ShareSource>,
        deadline: std::time::Instant,
        self_pid: u32,
    }

    unsafe fn is_window_cloaked(hwnd: HWND) -> bool {
        use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

        let mut cloaked: u32 = 0;
        let hr = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        hr.is_ok() && cloaked != 0
    }

    unsafe extern "system" fn window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam.0 as *mut EnumState);

        if std::time::Instant::now() > state.deadline {
            return BOOL(0);
        }

        let is_visible = IsWindowVisible(hwnd).as_bool();
        let is_minimized = IsIconic(hwnd).as_bool();

        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return BOOL(1);
        }

        if is_window_cloaked(hwnd) {
            return BOOL(1);
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let descriptor = WindowDescriptor {
            is_visible,
            is_minimized,
            client_area: measure_window_area(hwnd),
            process_id: pid,
        };
        if !should_include_window(&descriptor, state.self_pid) {
            return BOOL(1);
        }

        let title_len = GetWindowTextLengthW(hwnd);
        let name = if title_len > 0 {
            let mut buf = vec![0u16; (title_len + 1) as usize];
            let copied = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..copied as usize])
        } else {
            String::new()
        };

        if name.is_empty() {
            return BOOL(1);
        }

        let app_name = get_process_exe_name(pid);

        state.sources.push(ShareSource {
            id: format!("{}", hwnd.0 as isize),
            name,
            source_type: ShareSourceType::Window,
            thumbnail: None,
            app_name: Some(app_name),
        });

        BOOL(1)
    }

    unsafe fn get_process_exe_name(pid: u32) -> String {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        let handle = match handle {
            Ok(handle) => handle,
            Err(_) => return String::from("Unknown"),
        };

        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );

        let _ = windows::Win32::Foundation::CloseHandle(handle);

        if ok.is_err() || len == 0 {
            return String::from("Unknown");
        }

        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
    }

    let mut state = EnumState {
        sources: Vec::new(),
        deadline,
        self_pid,
    };

    let success = unsafe {
        EnumWindows(
            Some(window_callback),
            LPARAM(&mut state as *mut EnumState as isize),
        )
    };

    if success.is_err() && state.sources.is_empty() {
        if std::time::Instant::now() > deadline {
            return Ok(state.sources);
        }
        return Err("EnumWindows failed".to_string());
    }

    Ok(state.sources)
}

/// Enumerate windows as audio sources for per-process audio capture.
///
/// Each entry uses `pid:<PID>` so the capture thread can target a specific
/// process with `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`. A system-wide fallback
/// entry remains available as `system`.
fn enumerate_window_audio_sources(
    deadline: std::time::Instant,
) -> Result<Vec<ShareSource>, String> {
    use std::collections::HashMap;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    };

    let self_pid = std::process::id();

    struct EnumState {
        processes: HashMap<u32, (String, String)>,
        deadline: std::time::Instant,
        self_pid: u32,
    }

    unsafe fn is_window_cloaked_audio(hwnd: HWND) -> bool {
        use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

        let mut cloaked: u32 = 0;
        let hr = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        hr.is_ok() && cloaked != 0
    }

    unsafe extern "system" fn window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam.0 as *mut EnumState);

        if std::time::Instant::now() > state.deadline {
            return BOOL(0);
        }

        let is_visible = IsWindowVisible(hwnd).as_bool();
        let is_minimized = IsIconic(hwnd).as_bool();

        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return BOOL(1);
        }

        if is_window_cloaked_audio(hwnd) {
            return BOOL(1);
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return BOOL(1);
        }

        let descriptor = WindowDescriptor {
            is_visible,
            is_minimized,
            client_area: measure_window_area(hwnd),
            process_id: pid,
        };
        if !should_include_window(&descriptor, state.self_pid) {
            return BOOL(1);
        }

        let title_len = GetWindowTextLengthW(hwnd);
        let title = if title_len > 0 {
            let mut buf = vec![0u16; (title_len + 1) as usize];
            let copied = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..copied as usize])
        } else {
            String::new()
        };

        if title.is_empty() {
            return BOOL(1);
        }

        state.processes.entry(pid).or_insert_with(|| {
            let exe = get_process_exe_name_audio(pid);
            (exe, title)
        });

        BOOL(1)
    }

    unsafe fn get_process_exe_name_audio(pid: u32) -> String {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        let handle = match handle {
            Ok(handle) => handle,
            Err(_) => return String::from("Unknown"),
        };

        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );

        let _ = windows::Win32::Foundation::CloseHandle(handle);

        if ok.is_err() || len == 0 {
            return String::from("Unknown");
        }

        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
    }

    let mut state = EnumState {
        processes: HashMap::new(),
        deadline,
        self_pid,
    };

    let success = unsafe {
        EnumWindows(
            Some(window_callback),
            LPARAM(&mut state as *mut EnumState as isize),
        )
    };

    if success.is_err() && state.processes.is_empty() {
        if std::time::Instant::now() > deadline {
            return Ok(Vec::new());
        }
        return Err("EnumWindows failed for audio source enumeration".to_string());
    }

    let mut sources = Vec::with_capacity(state.processes.len() + 1);
    sources.push(ShareSource {
        id: "system".to_string(),
        name: "System Audio (all)".to_string(),
        source_type: ShareSourceType::SystemAudio,
        thumbnail: None,
        app_name: None,
    });

    let mut entries: Vec<_> = state.processes.into_iter().collect();
    entries.sort_by(|a, b| a.1 .0.to_lowercase().cmp(&b.1 .0.to_lowercase()));

    for (pid, (exe_name, window_title)) in entries {
        sources.push(ShareSource {
            id: format!("pid:{pid}"),
            name: window_title,
            source_type: ShareSourceType::SystemAudio,
            thumbnail: None,
            app_name: Some(exe_name),
        });
    }

    Ok(sources)
}

/// Capture a single RGBA frame from a Windows screen or window source via GDI.
fn capture_single_frame_windows(source_id: &str) -> Result<Option<(Vec<u8>, u32, u32)>, String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        GetDC, GetMonitorInfoW, GetWindowDC, ReleaseDC, SelectObject, HMONITOR, MONITORINFO, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};

    let handle_val: isize = match source_id.parse() {
        Ok(value) => value,
        Err(_) => {
            crate::debug_eprintln!("[share-thumb] source_id '{}' is not an isize handle", source_id);
            return Ok(None);
        }
    };

    unsafe {
        let hwnd = HWND(handle_val as *mut _);
        let mut client_rect = std::mem::zeroed();
        let got_rect = GetClientRect(hwnd, &mut client_rect);

        if got_rect.is_ok() {
            crate::debug_eprintln!("[share-thumb] capturing window handle {}", handle_val);
            let mut win_rect = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut win_rect).is_err() {
                crate::debug_eprintln!("[share-thumb] GetWindowRect failed for handle {}", handle_val);
                return Ok(None);
            }
            let w = (win_rect.right - win_rect.left) as u32;
            let h = (win_rect.bottom - win_rect.top) as u32;
            if w == 0 || h == 0 {
                crate::debug_eprintln!("[share-thumb] window {}x{} has zero dimension", w, h);
                return Ok(None);
            }

            let wnd_dc = GetWindowDC(hwnd);
            if wnd_dc.is_invalid() {
                crate::debug_eprintln!("[share-thumb] GetWindowDC failed");
                return Ok(None);
            }

            let mem_dc = CreateCompatibleDC(wnd_dc);
            if mem_dc.is_invalid() {
                ReleaseDC(hwnd, wnd_dc);
                crate::debug_eprintln!("[share-thumb] CreateCompatibleDC failed (window)");
                return Ok(None);
            }
            let bitmap = CreateCompatibleBitmap(wnd_dc, w as i32, h as i32);
            if bitmap.is_invalid() {
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(hwnd, wnd_dc);
                crate::debug_eprintln!("[share-thumb] CreateCompatibleBitmap failed (window)");
                return Ok(None);
            }
            let old_bmp = SelectObject(mem_dc, bitmap);
            let _ = BitBlt(mem_dc, 0, 0, w as i32, h as i32, wnd_dc, 0, 0, SRCCOPY);
            // Deselect before GetDIBits — GDI requires the bitmap not be selected
            // into any DC when GetDIBits reads its pixel data.
            SelectObject(mem_dc, old_bmp);
            let rgba = read_bitmap_rgba(bitmap, wnd_dc, w, h);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(hwnd, wnd_dc);
            crate::debug_eprintln!("[share-thumb] window capture {}x{}: rgba={}", w, h, rgba.is_some());

            return match rgba {
                Some(data) => Ok(Some((data, w, h))),
                None => Ok(None),
            };
        }

        // Not a window — try as monitor.
        crate::debug_eprintln!("[share-thumb] capturing monitor handle {}", handle_val);
        let monitor = HMONITOR(handle_val as *mut _);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..std::mem::zeroed()
        };
        if !GetMonitorInfoW(monitor, &mut mi).as_bool() {
            crate::debug_eprintln!("[share-thumb] GetMonitorInfoW failed for handle {}", handle_val);
            return Ok(None);
        }

        let w = (mi.rcMonitor.right - mi.rcMonitor.left) as u32;
        let h = (mi.rcMonitor.bottom - mi.rcMonitor.top) as u32;
        crate::debug_eprintln!("[share-thumb] monitor rect ({},{}) {}x{}", mi.rcMonitor.left, mi.rcMonitor.top, w, h);
        if w == 0 || h == 0 {
            crate::debug_eprintln!("[share-thumb] monitor has zero dimension");
            return Ok(None);
        }

        // GetDC(NULL) returns a DC for the entire virtual screen (all monitors),
        // which is required to BitBlt from secondary monitors at non-zero offsets.
        let screen_dc = GetDC(HWND::default());
        if screen_dc.is_invalid() {
            crate::debug_eprintln!("[share-thumb] GetDC(NULL) failed");
            return Ok(None);
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_invalid() {
            ReleaseDC(HWND::default(), screen_dc);
            crate::debug_eprintln!("[share-thumb] CreateCompatibleDC failed (monitor)");
            return Ok(None);
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, w as i32, h as i32);
        if bitmap.is_invalid() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), screen_dc);
            crate::debug_eprintln!("[share-thumb] CreateCompatibleBitmap failed (monitor)");
            return Ok(None);
        }
        let old_bmp = SelectObject(mem_dc, bitmap);
        let _ = BitBlt(
            mem_dc,
            0, 0,
            w as i32,
            h as i32,
            screen_dc,
            mi.rcMonitor.left,
            mi.rcMonitor.top,
            SRCCOPY,
        );
        // Deselect before GetDIBits — GDI requires the bitmap not be selected
        // into any DC when GetDIBits reads its pixel data.
        SelectObject(mem_dc, old_bmp);
        let rgba = read_bitmap_rgba(bitmap, screen_dc, w, h);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);
        crate::debug_eprintln!("[share-thumb] monitor capture {}x{}: rgba={}", w, h, rgba.is_some());

        match rgba {
            Some(data) => Ok(Some((data, w, h))),
            None => Ok(None),
        }
    }
}

/// Read BGRA pixel data from a GDI bitmap and convert it to RGBA.
/// The bitmap must NOT be selected into any DC before this is called.
unsafe fn read_bitmap_rgba(
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    dc: windows::Win32::Graphics::Gdi::HDC,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let screen_dc = dc;
    use windows::Win32::Graphics::Gdi::{
        GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..std::mem::zeroed()
        },
        ..std::mem::zeroed()
    };

    let mut bgra = vec![0u8; (w * h * 4) as usize];
    crate::debug_eprintln!("[share-thumb] GetDIBits: {}x{} ({} bytes)", w, h, bgra.len());
    let rows = GetDIBits(
        screen_dc,
        bitmap,
        0,
        h,
        Some(bgra.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );

    if rows == 0 {
        crate::debug_eprintln!("[share-thumb] GetDIBits returned 0 rows for {}x{}", w, h);
        return None;
    }
    crate::debug_eprintln!("[share-thumb] GetDIBits read {} rows for {}x{}", rows, w, h);

    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }

    // GDI BitBlt returns a valid (non-zero row count) but all-black bitmap for
    // GPU/DirectX-rendered windows it cannot read. Treat a fully-black frame as
    // a capture failure so callers show the name fallback instead.
    let is_all_black = bgra.chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);
    if is_all_black {
        crate::debug_eprintln!("[share-thumb] all-black frame detected, treating as capture failure");
        return None;
    }

    Some(bgra)
}

/// Fetch a thumbnail for a Windows source.
pub(super) fn fetch_thumbnail(source_id: &str) -> Result<Option<String>, String> {
    if source_id.contains('{') || source_id.starts_with("pid:") || source_id == "system" {
        return Ok(None);
    }
    if source_id.parse::<isize>().is_err() {
        return Ok(None);
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let source_id = source_id.to_string();

    std::thread::Builder::new()
        .name("win-thumb".into())
        .spawn(move || {
            let result = capture_single_frame_windows(&source_id);
            let _ = tx.send(result);
        })
        .map_err(|e| format!("failed to spawn thumbnail thread: {e}"))?;

    match rx.recv_timeout(THUMBNAIL_TIMEOUT) {
        Ok(Ok(Some((rgba, w, h)))) => encode_thumbnail_jpeg(&rgba, w, h),
        Ok(Ok(None)) => Ok(None),
        Ok(Err(e)) => {
            crate::debug_eprintln!("thumbnail capture failed: {e}");
            Ok(None)
        }
        Err(_) => {
            crate::debug_eprintln!("thumbnail fetch timed out");
            Ok(None)
        }
    }
}

/// Enumerate all Windows share sources within the picker latency budget.
pub(super) fn list_sources() -> Result<EnumerationResult, String> {
    let mut sources = Vec::new();
    let mut warnings = Vec::new();

    if !check_graphics_capture_available() {
        return Ok(EnumerationResult {
            sources: vec![],
            warnings: vec![
                "Windows Graphics Capture API unavailable (requires Windows 10 1803+)".into(),
            ],
            fallback_reason: Some(FallbackReason::GetDisplayMedia),
        });
    }

    let deadline = std::time::Instant::now() + WIN_ENUM_TIMEOUT;

    match enumerate_monitors(deadline) {
        Ok(screens) => sources.extend(screens),
        Err(e) => warnings.push(format!("Screen enumeration failed: {e}")),
    }

    match enumerate_windows(deadline) {
        Ok(windows) => sources.extend(windows),
        Err(e) => warnings.push(format!("Window enumeration failed: {e}")),
    }

    if std::time::Instant::now() > deadline {
        warnings.push("Enumeration exceeded 500ms budget - results may be incomplete".into());
    }

    let audio_deadline = std::time::Instant::now() + WIN_ENUM_TIMEOUT;
    match enumerate_window_audio_sources(audio_deadline) {
        Ok(audio) => sources.extend(audio),
        Err(e) => warnings.push(format!("Audio source enumeration unavailable: {e}")),
    }

    let fallback_reason = compute_fallback_reason(&sources);

    Ok(EnumerationResult {
        sources,
        warnings,
        fallback_reason,
    })
}
