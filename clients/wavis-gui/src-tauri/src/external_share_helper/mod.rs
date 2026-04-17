#![cfg(target_os = "linux")]
//! Owns the external share helper's HTTP server, browser coordination, and
//! single-session lifecycle. Frame decoding and LiveKit video publication live
//! in the `pipewire_frame_processor` submodule.

mod pipewire_frame_processor;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::ShellExt;
use wavis_client_shared::room_session::LiveKitConnection;

use crate::media::MediaState;

const LOG: &str = "[wavis:external-share-helper]";

pub struct ExternalShareHelperState {
    inner: Arc<Mutex<Inner>>,
}

pub(super) struct Inner {
    port: Option<u16>,
    active_session: Option<HelperSession>,
}

#[derive(Clone)]
pub(super) struct HelperSession {
    id: String,
    stop_requested: bool,
    published: bool,
    /// True while `publish_video` is in progress. Other threads must wait or
    /// skip frame feeding until this becomes false.
    publishing_in_progress: bool,
}

impl ExternalShareHelperState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                port: None,
                active_session: None,
            })),
        }
    }

    fn ensure_server(&self, app: AppHandle) -> Result<u16, String> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| format!("external helper lock: {e}"))?;
        if let Some(port) = guard.port {
            return Ok(port);
        }

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("failed to bind helper server: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("failed to read helper server address: {e}"))?
            .port();

        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("external-share-helper".into())
            .spawn(move || run_server(listener, inner, app))
            .map_err(|e| format!("failed to spawn helper server: {e}"))?;

        guard.port = Some(port);
        Ok(port)
    }
}

fn run_server(listener: TcpListener, inner: Arc<Mutex<Inner>>, app: AppHandle) {
    log::info!(
        "{LOG} helper server listening on 127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let inner = Arc::clone(&inner);
        let app = app.clone();
        let _ = std::thread::Builder::new()
            .name("external-share-helper-client".into())
            .spawn(move || {
                if let Err(e) = handle_connection(stream, inner, app) {
                    log::warn!("{LOG} connection handler error: {e}");
                }
            });
    }
}

fn handle_connection(
    mut stream: TcpStream,
    inner: Arc<Mutex<Inner>>,
    app: AppHandle,
) -> Result<(), String> {
    let mut req_bytes = Vec::with_capacity(16384);
    let mut header_end = None;
    let mut content_length = 0usize;

    while header_end.is_none() {
        let mut chunk = [0u8; 4096];
        let read = stream
            .read(&mut chunk)
            .map_err(|e| format!("helper read failed: {e}"))?;
        if read == 0 {
            break;
        }
        req_bytes.extend_from_slice(&chunk[..read]);
        header_end = find_header_end(&req_bytes);
        if let Some(end) = header_end {
            let header_text = String::from_utf8_lossy(&req_bytes[..end]);
            content_length = header_text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            break;
        }
    }

    if req_bytes.is_empty() {
        return Ok(());
    }

    let header_end =
        header_end.ok_or_else(|| "helper request missing header terminator".to_string())?;
    let req = String::from_utf8_lossy(&req_bytes[..header_end]);
    let mut lines = req.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| "empty helper request".to_string())?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");

    let mut body = req_bytes[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut extra = vec![0u8; content_length - body.len()];
        let extra_read = stream
            .read(&mut extra)
            .map_err(|e| format!("helper body read failed: {e}"))?;
        if extra_read == 0 {
            break;
        }
        body.extend_from_slice(&extra[..extra_read]);
    }

    let (path, query) = split_target(target);

    match (method, path.as_str()) {
        ("GET", "/helper") => respond_html(
            &mut stream,
            &helper_html(query_param(&query, "session").unwrap_or_default()),
        )?,
        ("GET", "/api/session") => {
            let session_id = query_param(&query, "session").unwrap_or_default();
            let exists = inner
                .lock()
                .ok()
                .and_then(|g| g.active_session.clone())
                .filter(|s| s.id == session_id)
                .is_some();
            if exists {
                let payload = serde_json::json!({ "sessionId": session_id });
                respond_json(&mut stream, 200, &payload.to_string())?;
            } else {
                respond_json(&mut stream, 404, "{\"error\":\"session not found\"}")?;
            }
        }
        ("GET", "/api/control") => {
            let session_id = query_param(&query, "session").unwrap_or_default();
            let stop_requested = inner
                .lock()
                .ok()
                .and_then(|g| g.active_session.clone())
                .filter(|s| s.id == session_id)
                .map(|s| s.stop_requested)
                .unwrap_or(true);
            let payload = serde_json::json!({ "stopRequested": stop_requested });
            respond_json(&mut stream, 200, &payload.to_string())?;
        }
        ("POST", "/api/event") => {
            let payload: HelperEvent = serde_json::from_slice(&body)
                .map_err(|e| format!("invalid helper event payload: {e}"))?;
            process_helper_event(payload, &inner, &app);
            respond_json(&mut stream, 200, "{\"ok\":true}")?;
        }
        ("POST", "/api/frame") => {
            let session_id = query_param(&query, "session").unwrap_or_default();
            pipewire_frame_processor::process_helper_frame(&session_id, body, &inner, &app)?;
            respond_json(&mut stream, 200, "{\"ok\":true}")?;
        }
        _ => respond_json(&mut stream, 404, "{\"error\":\"not found\"}")?,
    }

    Ok(())
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn respond_html(stream: &mut TcpStream, html: &str) -> Result<(), String> {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    stream
        .write_all(resp.as_bytes())
        .map_err(|e| format!("helper write failed: {e}"))
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), String> {
    let status_text = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        body.len(),
        body
    );
    stream
        .write_all(resp.as_bytes())
        .map_err(|e| format!("helper write failed: {e}"))
}

fn split_target(target: &str) -> (String, String) {
    if let Some((path, query)) = target.split_once('?') {
        (path.to_string(), query.to_string())
    } else {
        (target.to_string(), String::new())
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            Some(v.to_string())
        } else {
            None
        }
    })
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HelperEvent {
    session_id: String,
    event: String,
    message: Option<String>,
}

fn process_helper_event(payload: HelperEvent, inner: &Arc<Mutex<Inner>>, app: &AppHandle) {
    log::debug!(
        "{LOG} event: type={} session_id={} message={:?}",
        payload.event,
        payload.session_id,
        payload.message
    );
    let mut guard = match inner.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(session) = guard.active_session.clone() else {
        log::debug!("{LOG} event ignored: no active session");
        return;
    };
    if session.id != payload.session_id {
        log::warn!(
            "{LOG} event ignored: session mismatch (active={} event={})",
            session.id,
            payload.session_id
        );
        return;
    }

    match payload.event.as_str() {
        "started" => {
            log::info!("{LOG} session started: {}", payload.session_id);
            let _ = app.emit(
                "external-share-started",
                serde_json::json!({ "sessionId": payload.session_id }),
            );
        }
        "stopped" => {
            log::info!(
                "{LOG} session stopped: {} (published={})",
                payload.session_id,
                session.published
            );
            let _ = pipewire_frame_processor::stop_published_video(app, &mut guard);
            let _ = app.emit(
                "external-share-stopped",
                serde_json::json!({ "sessionId": payload.session_id }),
            );
            guard.active_session = None;
        }
        "error" => {
            log::warn!(
                "{LOG} session error: {} msg={:?}",
                payload.session_id,
                payload.message
            );
            let _ = pipewire_frame_processor::stop_published_video(app, &mut guard);
            let _ = app.emit(
                "external-share-error",
                serde_json::json!({
                    "sessionId": payload.session_id,
                    "message": payload
                        .message
                        .unwrap_or_else(|| "external share failed".to_string()),
                }),
            );
            guard.active_session = None;
        }
        _ => {}
    }
}

fn helper_html(session_id: String) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Wavis Share Helper</title>
  <style>
    body {{ font-family: sans-serif; background: #0e1116; color: #e7edf5; margin: 0; }}
    main {{ max-width: 720px; margin: 48px auto; padding: 24px; }}
    .card {{ background: #161b22; border: 1px solid #2d3748; border-radius: 12px; padding: 24px; }}
    button {{ background: #5eead4; color: #081018; border: 0; border-radius: 999px; padding: 12px 18px; font-weight: 700; cursor: pointer; }}
    button:disabled {{ opacity: 0.5; cursor: default; }}
    .muted {{ color: #94a3b8; }}
    .err {{ color: #fca5a5; white-space: pre-wrap; }}
  </style>
</head>
<body>
  <main>
    <div class="card">
      <h1>Wavis Share Helper</h1>
      <p class="muted">This helper runs in your system browser, captures the screen with getDisplayMedia, and streams frames back to the already-connected Wavis app so room audio stays intact.</p>
      <p id="status">Ready.</p>
      <p id="error" class="err"></p>
      <button id="start">Start screen share</button>
      <button id="stop" disabled>Stop</button>
    </div>
  </main>
  <script type="module">
    const sessionId = {session_id:?};
    const statusEl = document.getElementById('status');
    const errorEl = document.getElementById('error');
    const startBtn = document.getElementById('start');
    const stopBtn = document.getElementById('stop');
    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d', {{ alpha: false, desynchronized: true }});
    let stream = null;
    let video = null;
    let started = false;
    let stopped = false;
    let frameLoopTimer = null;
    let frameInFlight = false;

    const notify = async (event, message = null) => {{
      try {{
        await fetch('/api/event', {{
          method: 'POST',
          headers: {{ 'Content-Type': 'application/json' }},
          body: JSON.stringify({{ sessionId, event, message }}),
        }});
      }} catch {{}}
    }};

    const sendFrame = async () => {{
      if (stopped || !video || frameInFlight) return;
      if (video.videoWidth < 2 || video.videoHeight < 2) return;
      if (!ctx) throw new Error('2D canvas context unavailable');
      frameInFlight = true;
      try {{
        let dw = video.videoWidth;
        let dh = video.videoHeight;
        const MAX_W = 1920;
        const MAX_H = 1080;
        if (dw > MAX_W || dh > MAX_H) {{
          const scale = Math.min(MAX_W / dw, MAX_H / dh);
          dw = Math.max(1, Math.round(dw * scale));
          dh = Math.max(1, Math.round(dh * scale));
        }}
        if (canvas.width !== dw || canvas.height !== dh) {{
          canvas.width = dw;
          canvas.height = dh;
        }}
        ctx.drawImage(video, 0, 0, dw, dh);
        const blob = await new Promise((resolve, reject) => {{
          canvas.toBlob((value) => {{
            if (value) resolve(value);
            else reject(new Error('failed to encode frame'));
          }}, 'image/png');
        }});
        const res = await fetch(`/api/frame?session=${{encodeURIComponent(sessionId)}}`, {{
          method: 'POST',
          headers: {{ 'Content-Type': 'image/png' }},
          cache: 'no-store',
          body: blob,
        }});
        if (!res.ok) {{
          const text = await res.text().catch(() => '');
          throw new Error(text || `frame upload failed (${{res.status}})`);
        }}
      }} finally {{
        frameInFlight = false;
      }}
    }};

    const frameLoop = async () => {{
      if (stopped) return;
      try {{
        await sendFrame();
        frameLoopTimer = window.setTimeout(frameLoop, 100);
      }} catch (err) {{
        const message = err instanceof Error ? err.message : String(err);
        errorEl.textContent = message;
        await notify('error', message);
        await stopShare(false);
      }}
    }};

    const stopTracks = () => {{
      if (stream) {{
        for (const track of stream.getTracks()) {{
          try {{ track.stop(); }} catch {{}}
        }}
      }}
      if (video) {{
        video.srcObject = null;
        video = null;
      }}
      stream = null;
    }};

    const stopShare = async (emitStopped = true) => {{
      if (stopped) return;
      stopped = true;
      statusEl.textContent = 'Stopping share...';
      if (frameLoopTimer !== null) {{
        clearTimeout(frameLoopTimer);
        frameLoopTimer = null;
      }}
      stopTracks();
      if (started && emitStopped) {{
        await notify('stopped');
      }}
      statusEl.textContent = 'Share stopped. You can close this tab.';
      startBtn.disabled = false;
      stopBtn.disabled = true;
      started = false;
    }};

    const controlTimer = setInterval(async () => {{
      if (!started || stopped) return;
      try {{
        const res = await fetch(`/api/control?session=${{encodeURIComponent(sessionId)}}`, {{ cache: 'no-store' }});
        const body = await res.json();
        if (body.stopRequested) {{
          await stopShare(false);
          window.close();
        }}
      }} catch {{}}
    }}, 1000);

    window.addEventListener('beforeunload', () => {{
      clearInterval(controlTimer);
      if (started && !stopped) {{
        navigator.sendBeacon('/api/event', new Blob([JSON.stringify({{ sessionId, event: 'stopped' }})], {{ type: 'application/json' }}));
      }}
      stopTracks();
    }});

    startBtn.addEventListener('click', async () => {{
      errorEl.textContent = '';
      statusEl.textContent = 'Requesting screen share permission...';
      startBtn.disabled = true;
      stopped = false;

      try {{
        const sessionRes = await fetch(`/api/session?session=${{encodeURIComponent(sessionId)}}`, {{ cache: 'no-store' }});
        if (!sessionRes.ok) throw new Error('helper session expired');

        stream = await navigator.mediaDevices.getDisplayMedia({{
          video: {{
            frameRate: 15,
            width: {{ ideal: 1920 }},
            height: {{ ideal: 1080 }},
          }},
          audio: false,
        }});

        const [track] = stream.getVideoTracks();
        if (!track) throw new Error('no video track returned by screen capture');

        track.addEventListener('ended', () => {{
          stopShare();
        }});

        video = document.createElement('video');
        video.srcObject = stream;
        video.muted = true;
        video.playsInline = true;
        await video.play();

        started = true;
        stopBtn.disabled = false;
        statusEl.textContent = 'Screen share is live. Keep this tab open while sharing.';
        await notify('started');
        frameLoop();
      }} catch (err) {{
        const message = err instanceof Error ? err.message : String(err);
        errorEl.textContent = message;
        statusEl.textContent = 'Share failed.';
        startBtn.disabled = false;
        stopBtn.disabled = true;
        stopTracks();
        await notify('error', message);
      }}
    }});

    stopBtn.addEventListener('click', async () => {{
      await stopShare();
    }});
  </script>
</body>
</html>
"#
    )
}

#[tauri::command]
pub fn external_share_start(
    state: tauri::State<'_, ExternalShareHelperState>,
    app: AppHandle,
) -> Result<(), String> {
    {
        let media_state = app
            .try_state::<MediaState>()
            .ok_or_else(|| "media state unavailable".to_string())?;
        let lk_guard = media_state.lk().map_err(|e| format!("lock: {e}"))?;
        let conn = lk_guard
            .as_ref()
            .ok_or_else(|| "not connected to a room".to_string())?;
        if !conn.is_available() {
            return Err("not connected to a room".to_string());
        }
    }

    let port = state.ensure_server(app.clone())?;
    let session_id = format!(
        "helper-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    log::info!("{LOG} external_share_start: new session={session_id}");

    {
        let mut guard = state
            .inner
            .lock()
            .map_err(|e| format!("external helper lock: {e}"))?;
        if let Some(old) = guard.active_session.as_ref() {
            log::warn!(
                "{LOG} replacing stale session: old={} published={} stop_requested={}",
                old.id,
                old.published,
                old.stop_requested
            );
            // Recover from stale sessions (for example, a helper tab that
            // closed before it could emit `stopped` or `error`). We preserve
            // single-session semantics by shutting down the old published track
            // before replacing the session.
            let _ = pipewire_frame_processor::stop_published_video(&app, &mut guard);
            guard.active_session = None;
        }
        guard.active_session = Some(HelperSession {
            id: session_id.clone(),
            stop_requested: false,
            published: false,
            publishing_in_progress: false,
        });
    }

    let url = format!("http://127.0.0.1:{port}/helper?session={session_id}");
    #[allow(deprecated)]
    app.shell()
        .open(url, None)
        .map_err(|e| format!("failed to open system browser: {e}"))?;

    log::info!("{LOG} opened external helper session {session_id}");
    Ok(())
}

#[tauri::command]
pub fn external_share_stop(
    state: tauri::State<'_, ExternalShareHelperState>,
    app: AppHandle,
) -> Result<(), String> {
    log::debug!("{LOG} external_share_stop called");
    let mut guard = state
        .inner
        .lock()
        .map_err(|e| format!("external helper lock: {e}"))?;
    if let Some(session) = guard.active_session.as_mut() {
        log::info!(
            "{LOG} stopping session: {} published={}",
            session.id,
            session.published
        );
        session.stop_requested = true;
    } else {
        log::debug!("{LOG} external_share_stop: no active session");
    }
    let _ = pipewire_frame_processor::stop_published_video(&app, &mut guard);
    Ok(())
}
