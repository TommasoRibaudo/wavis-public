//! Direct PipeWire source capture — targets a specific node ID without the portal picker.
//!
//! Used by `screen_share_start_source` to capture a screen or window that was
//! previously enumerated by `list_share_sources`. On Wayland the portal-authorized
//! PipeWire fd is used; on X11 a direct PipeWire connection is made.
//!
//! Registers a `state_changed` callback on the PipeWire stream to detect when
//! the captured source disappears (e.g. shared window closed by the OS) and
//! emits a `share_error` Tauri event so the frontend can clean up.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pipewire as pw;
use tauri::{AppHandle, Emitter};

use super::{ensure_pipewire_init, CaptureError, CapturedFrame, ScreenCapture};

const LOG: &str = "[wavis:source-capture]";
type FrameCallback = Arc<Mutex<Option<Box<dyn Fn(CapturedFrame) + Send + 'static>>>>;

/// Configuration for creating a direct PipeWire source capture.
pub struct SourceCaptureConfig {
    /// PipeWire node ID to capture.
    pub node_id: u32,
    /// Portal-authorized PipeWire fd (Wayland). `None` on X11 — uses direct connect.
    pub pw_fd: Option<OwnedFd>,
    /// Tauri app handle for emitting `share_error` events.
    pub app_handle: AppHandle,
}

/// Direct PipeWire capture backend targeting a specific node ID.
///
/// Unlike `PipeWireCapture` (which goes through the portal picker), this backend
/// connects to a known node ID directly. It implements `ScreenCapture` so it can
/// be stored in `MediaState.screen_capture` and stopped via `screen_share_stop`.
pub struct SourceCapture {
    active: Arc<AtomicBool>,
    frame_callback: FrameCallback,
    pw_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    quit_tx: Mutex<Option<pw::channel::Sender<()>>>,
}

impl SourceCapture {
    /// Create and start a source capture for the given configuration.
    ///
    /// Returns a started `SourceCapture` that implements `ScreenCapture`.
    /// The caller should register a frame callback via `on_frame()` and store
    /// the capture in `MediaState.screen_capture`.
    pub fn start(config: SourceCaptureConfig) -> Result<Self, CaptureError> {
        let capture = Self {
            active: Arc::new(AtomicBool::new(true)),
            frame_callback: Arc::new(Mutex::new(None)),
            pw_thread: Mutex::new(None),
            quit_tx: Mutex::new(None),
        };

        let active = capture.active.clone();
        let frame_cb = capture.frame_callback.clone();

        let (quit_tx, quit_rx) = pw::channel::channel::<()>();
        *capture.quit_tx.lock().unwrap() = Some(quit_tx);

        let handle = std::thread::Builder::new()
            .name("pw-source-capture".into())
            .spawn(move || {
                Self::pw_loop_main(config, active, frame_cb, quit_rx);
            })
            .map_err(|e| {
                CaptureError::CaptureStartFailed(format!(
                    "failed to spawn PipeWire source capture thread: {e}"
                ))
            })?;

        *capture.pw_thread.lock().unwrap() = Some(handle);
        Ok(capture)
    }

    /// PipeWire main loop — runs on a dedicated thread.
    fn pw_loop_main(
        config: SourceCaptureConfig,
        active: Arc<AtomicBool>,
        frame_cb: FrameCallback,
        quit_rx: pw::channel::Receiver<()>,
    ) {
        ensure_pipewire_init();

        let mainloop = match pw::main_loop::MainLoopRc::new(None) {
            Ok(ml) => ml,
            Err(e) => {
                log::error!("{LOG} MainLoop creation failed: {e}");
                active.store(false, Ordering::SeqCst);
                Self::emit_share_error(
                    &config.app_handle,
                    "PipeWire connection lost — capture stopped",
                );
                return;
            }
        };

        let context = match pw::context::ContextRc::new(&mainloop, None) {
            Ok(ctx) => ctx,
            Err(e) => {
                log::error!("{LOG} Context creation failed: {e}");
                active.store(false, Ordering::SeqCst);
                Self::emit_share_error(
                    &config.app_handle,
                    "PipeWire connection lost — capture stopped",
                );
                return;
            }
        };

        // Connect using the portal fd on Wayland, or directly on X11.
        let core = match config.pw_fd {
            Some(fd) => context.connect_fd_rc(fd, None),
            None => context.connect_rc(None),
        };
        let core = match core {
            Ok(c) => c,
            Err(e) => {
                log::error!("{LOG} PipeWire connect failed: {e}");
                active.store(false, Ordering::SeqCst);
                Self::emit_share_error(
                    &config.app_handle,
                    "PipeWire connection lost — capture stopped",
                );
                return;
            }
        };

        // Attach the quit receiver so we can break out of the main loop.
        let mainloop_weak_quit = mainloop.downgrade();
        let _quit_listener = quit_rx.attach(mainloop.loop_(), move |_| {
            if let Some(ml) = mainloop_weak_quit.upgrade() {
                ml.quit();
            }
        });

        let stream = match pw::stream::StreamRc::new(
            core.clone(),
            "wavis-source-capture",
            pw::properties::properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Screen",
            },
        ) {
            Ok(s) => s,
            Err(e) => {
                log::error!("{LOG} Stream creation failed: {e}");
                active.store(false, Ordering::SeqCst);
                Self::emit_share_error(
                    &config.app_handle,
                    "Capture failed — the source may no longer be available",
                );
                return;
            }
        };

        // Register stream state change callback for window close detection.
        // When the source node is destroyed (window closed), PipeWire transitions
        // the stream to Error or Unconnected.
        let active_state = active.clone();
        let app_handle_state = config.app_handle.clone();
        let mainloop_weak_state = mainloop.downgrade();
        // Track whether we've been connected at least once — an Unconnected state
        // is only an error if we were previously streaming.
        let was_streaming = Arc::new(AtomicBool::new(false));
        let was_streaming_process = was_streaming.clone();

        // Register both state_changed and process callbacks.
        let active_process = active.clone();
        let _listener = stream
            .add_local_listener()
            .state_changed(move |_, _, _old, new| {
                log::debug!("{LOG} stream state changed: {new:?}");
                match new {
                    pw::stream::StreamState::Streaming => {
                        was_streaming.store(true, Ordering::SeqCst);
                    }
                    pw::stream::StreamState::Error(_) => {
                        log::warn!("{LOG} stream entered error state");
                        active_state.store(false, Ordering::SeqCst);
                        Self::emit_share_error(
                            &app_handle_state,
                            "Capture failed — the source may no longer be available",
                        );
                        if let Some(ml) = mainloop_weak_state.upgrade() {
                            ml.quit();
                        }
                    }
                    pw::stream::StreamState::Unconnected => {
                        // Only treat as error if we were previously streaming.
                        if was_streaming.load(Ordering::SeqCst) {
                            log::warn!("{LOG} stream disconnected (source closed)");
                            active_state.store(false, Ordering::SeqCst);
                            Self::emit_share_error(&app_handle_state, "Shared window was closed");
                            if let Some(ml) = mainloop_weak_state.upgrade() {
                                ml.quit();
                            }
                        }
                    }
                    _ => {}
                }
            })
            .process(move |stream, _user_data: &mut ()| {
                if !active_process.load(Ordering::SeqCst) {
                    return;
                }
                was_streaming_process.store(true, Ordering::SeqCst);
                Self::on_pw_process(stream, &frame_cb);
            })
            .register();

        // Connect the stream to the specific node ID.
        let flags = pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
        if let Err(e) = stream.connect(
            pw::spa::utils::Direction::Input,
            Some(config.node_id),
            flags,
            &mut [],
        ) {
            log::error!(
                "{LOG} stream connect failed for node {}: {e}",
                config.node_id
            );
            active.store(false, Ordering::SeqCst);
            Self::emit_share_error(
                &config.app_handle,
                "Capture failed — the source may no longer be available",
            );
            return;
        }

        log::info!("{LOG} capturing node {}", config.node_id);

        // Run the main loop — blocks until quit is signalled or stream errors.
        mainloop.run();

        // Cleanup.
        let _ = stream.disconnect();
        active.store(false, Ordering::SeqCst);
        log::info!("{LOG} capture loop exited for node {}", config.node_id);
    }

    /// Emit a `share_error` Tauri event to the frontend.
    fn emit_share_error(app: &AppHandle, message: &str) {
        #[derive(Clone, serde::Serialize)]
        struct ShareError {
            message: String,
        }
        if let Err(e) = app.emit(
            "share_error",
            ShareError {
                message: message.to_string(),
            },
        ) {
            log::warn!("{LOG} failed to emit share_error event: {e}");
        }
    }

    /// Extract frame data from a PipeWire buffer — same logic as `PipeWireCapture::on_pw_process`.
    fn on_pw_process(stream: &pw::stream::Stream, frame_cb: &FrameCallback) {
        let buffer_ptr = unsafe { stream.dequeue_raw_buffer() };
        if buffer_ptr.is_null() {
            return;
        }

        let pw_buf = unsafe { &*buffer_ptr };
        let spa_buf = unsafe { &*pw_buf.buffer };

        if spa_buf.n_datas == 0 {
            unsafe { stream.queue_raw_buffer(buffer_ptr) };
            return;
        }

        let data_ref = unsafe { &*spa_buf.datas.add(0) };
        let data_ptr = data_ref.data;
        if data_ptr.is_null() {
            unsafe { stream.queue_raw_buffer(buffer_ptr) };
            return;
        }

        let chunk = unsafe { &*data_ref.chunk };
        let size = chunk.size as usize;
        let stride = chunk.stride as usize;

        if size == 0 || stride == 0 {
            unsafe { stream.queue_raw_buffer(buffer_ptr) };
            return;
        }

        let bytes_per_pixel = 4usize;
        let width = stride / bytes_per_pixel;
        let height = size / stride;

        if width == 0 || height == 0 {
            unsafe { stream.queue_raw_buffer(buffer_ptr) };
            return;
        }

        // Copy raw pixel data, converting BGRx → RGBA.
        let raw_slice = unsafe { std::slice::from_raw_parts(data_ptr as *const u8, size) };

        let mut rgba = Vec::with_capacity(width * height * 4);
        for row in 0..height {
            let row_start = row * stride;
            for col in 0..width {
                let px = row_start + col * bytes_per_pixel;
                if px + 3 > raw_slice.len() {
                    break;
                }
                rgba.push(raw_slice[px + 2]); // R (was B)
                rgba.push(raw_slice[px + 1]); // G
                rgba.push(raw_slice[px]); // B (was R)
                rgba.push(255); // A
            }
        }

        unsafe { stream.queue_raw_buffer(buffer_ptr) };

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let frame = CapturedFrame {
            width: width as u32,
            height: height as u32,
            data: rgba,
            timestamp_ms,
        };

        if let Ok(guard) = frame_cb.lock() {
            if let Some(cb) = guard.as_ref() {
                cb(frame);
            }
        }
    }
}

impl ScreenCapture for SourceCapture {
    fn start(&self) -> Result<(), CaptureError> {
        // SourceCapture is started via `SourceCapture::start(config)` — this is a no-op
        // since the stream is already running when the struct is constructed.
        Ok(())
    }

    fn stop(&self) {
        if !self.active.load(Ordering::SeqCst) {
            return;
        }

        self.active.store(false, Ordering::SeqCst);

        // Signal the PipeWire main loop to quit.
        if let Ok(mut guard) = self.quit_tx.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }

        // Wait for the PipeWire thread to finish.
        if let Ok(mut guard) = self.pw_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }

        log::info!("{LOG} source capture stopped");
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    fn on_frame(&self, cb: Box<dyn Fn(CapturedFrame) + Send + 'static>) {
        if let Ok(mut guard) = self.frame_callback.lock() {
            *guard = Some(cb);
        }
    }
}
