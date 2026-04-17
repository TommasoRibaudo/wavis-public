//! PipeWire and PulseAudio share-source enumeration and thumbnail capture (Linux).
//!
//! This module owns direct PipeWire and PulseAudio integration for native share
//! source discovery. Portal fallback policy, shared DTOs, and thumbnail
//! encoding stay in the parent `share_sources` module.

use crate::screen_capture::ensure_pipewire_init;

use super::super::{
    compute_fallback_reason, encode_thumbnail_jpeg, EnumerationResult, ShareSource,
    ShareSourceType, THUMBNAIL_TIMEOUT,
};

/// Timeout for PipeWire enumeration so the picker does not hang on a stalled daemon.
const PW_ENUM_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

const SCREEN_MEDIA_CLASSES: &[&str] = &[
    "Video/Source/DRM",
    "Video/Source/KMS",
    "Video/Source/Virtual/Screen",
];

const WINDOW_MEDIA_CLASSES: &[&str] = &[
    "Video/Source/Window",
    "Video/Source/Virtual/Window",
    "Stream/Output/Video",
];

/// Classify a PipeWire node's `media.class` into a shareable video source type.
pub(in crate::share_sources) fn classify_video_node(media_class: &str) -> Option<ShareSourceType> {
    if SCREEN_MEDIA_CLASSES.contains(&media_class) {
        return Some(ShareSourceType::Screen);
    }
    if WINDOW_MEDIA_CLASSES.contains(&media_class) {
        return Some(ShareSourceType::Window);
    }

    let lower = media_class.to_ascii_lowercase();
    if lower.contains("drm") || lower.contains("kms") || lower.contains("monitor") {
        return Some(ShareSourceType::Screen);
    }
    if lower.contains("window")
        || lower.contains("toplevel")
        || lower.contains("xdg")
        || lower.contains("stream/output/video")
    {
        return Some(ShareSourceType::Window);
    }
    if media_class == "Video/Source" {
        return Some(ShareSourceType::Screen);
    }

    None
}

/// Run PipeWire enumeration on a dedicated thread.
fn enumerate_pipewire_sources() -> Result<Vec<ShareSource>, String> {
    use std::sync::mpsc;

    crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_sources: spawning thread");
    let (tx, rx) = mpsc::channel();

    let handle = std::thread::Builder::new()
        .name("pw-enum".into())
        .spawn(move || {
            crate::debug_eprintln!(
                "wavis: share_sources: enumerate_pipewire_sources: thread entered"
            );
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(enumerate_pipewire_inner));
            let result = match result {
                Ok(result) => result,
                Err(payload) => {
                    let panic_msg = if let Some(msg) = payload.downcast_ref::<&str>() {
                        (*msg).to_string()
                    } else if let Some(msg) = payload.downcast_ref::<String>() {
                        msg.clone()
                    } else {
                        "unknown panic payload".to_string()
                    };
                    Err(format!(
                        "PipeWire daemon unavailable: enumeration panicked: {panic_msg}"
                    ))
                }
            };
            let _ = tx.send(result);
        })
        .map_err(|e| {
            format!("PipeWire daemon unavailable: failed to spawn enumeration thread: {e}")
        })?;

    match rx.recv_timeout(PW_ENUM_TIMEOUT) {
        Ok(result) => {
            crate::debug_eprintln!(
                "wavis: share_sources: enumerate_pipewire_sources: thread returned"
            );
            let _ = handle.join();
            result
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_sources: timeout");
            Err("PipeWire daemon unavailable: enumeration timed out".to_string())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            crate::debug_eprintln!(
                "wavis: share_sources: enumerate_pipewire_sources: disconnected"
            );
            let _ = handle.join();
            Err("PipeWire daemon unavailable: enumeration thread panicked".to_string())
        }
    }
}

/// Inner PipeWire enumeration logic. Must run on a dedicated thread.
fn enumerate_pipewire_inner() -> Result<Vec<ShareSource>, String> {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use pipewire as pw;

    crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_inner: pw::init");
    ensure_pipewire_init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| format!("PipeWire daemon unavailable: failed to create main loop: {e}"))?;
    crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_inner: main loop created");

    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| format!("PipeWire daemon unavailable: failed to create context: {e}"))?;
    crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_inner: context created");

    let core = context
        .connect_rc(None)
        .map_err(|e| format!("PipeWire daemon unavailable: {e}"))?;
    crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_inner: core connected");

    let registry = core
        .get_registry()
        .map_err(|e| format!("PipeWire daemon unavailable: failed to get registry: {e}"))?;
    crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_inner: registry acquired");

    let sources: Rc<RefCell<Vec<ShareSource>>> = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(Cell::new(false));

    let pending = core
        .sync(0)
        .map_err(|e| format!("PipeWire daemon unavailable: sync failed: {e}"))?;
    crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_inner: sync requested");

    let done_clone = done.clone();
    let loop_clone = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_clone.set(true);
                loop_clone.quit();
            }
        })
        .register();

    let sources_clone = sources.clone();
    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }

            let props = match global.props.as_ref() {
                Some(props) => props,
                None => return,
            };

            let media_class = match props.get(*pw::keys::MEDIA_CLASS) {
                Some(media_class) => media_class,
                None => return,
            };

            let source_type = match classify_video_node(media_class) {
                Some(source_type) => source_type,
                None => return,
            };

            let name = props
                .get(*pw::keys::NODE_DESCRIPTION)
                .or_else(|| props.get(*pw::keys::NODE_NAME))
                .unwrap_or("Unknown source")
                .to_string();

            let app_name = if source_type == ShareSourceType::Window {
                Some(
                    props
                        .get(*pw::keys::APP_NAME)
                        .or_else(|| props.get("application.name"))
                        .unwrap_or("Unknown app")
                        .to_string(),
                )
            } else {
                None
            };

            let id = global.id.to_string();

            sources_clone.borrow_mut().push(ShareSource {
                id,
                name,
                source_type,
                thumbnail: None,
                app_name,
            });
        })
        .register();

    while !done.get() {
        crate::debug_eprintln!("wavis: share_sources: enumerate_pipewire_inner: mainloop.run");
        mainloop.run();
    }

    let result = sources.borrow().clone();
    crate::debug_eprintln!(
        "wavis: share_sources: enumerate_pipewire_inner: collected {} sources",
        result.len()
    );
    Ok(result)
}

/// Timeout for PulseAudio enumeration to avoid hanging on an unresponsive daemon.
const PA_ENUM_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

/// Enumerate PulseAudio monitor sources using the threaded mainloop pattern.
fn enumerate_pulseaudio_sources() -> Result<Vec<ShareSource>, String> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = std::thread::Builder::new()
        .name("pa-enum".into())
        .spawn(move || {
            let result = enumerate_pulseaudio_inner();
            let _ = tx.send(result);
        })
        .map_err(|e| format!("PulseAudio unavailable: failed to spawn enumeration thread: {e}"))?;

    match rx.recv_timeout(PA_ENUM_TIMEOUT) {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err("PulseAudio unavailable: enumeration timed out".to_string())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            Err("PulseAudio unavailable: enumeration thread panicked".to_string())
        }
    }
}

/// Inner PulseAudio enumeration logic. Must run on a dedicated thread.
fn enumerate_pulseaudio_inner() -> Result<Vec<ShareSource>, String> {
    use std::sync::{Arc, Mutex};

    use pulse::callbacks::ListResult;
    use pulse::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
    use pulse::mainloop::threaded::Mainloop;

    let mut mainloop = Mainloop::new()
        .ok_or_else(|| "PulseAudio unavailable: failed to create mainloop".to_string())?;

    mainloop
        .start()
        .map_err(|e| format!("PulseAudio unavailable: failed to start mainloop: {e}"))?;

    let mut context = Context::new(&mainloop, "wavis-source-enum")
        .ok_or_else(|| "PulseAudio unavailable: failed to create context".to_string())?;

    mainloop.lock();
    context
        .connect(None, ContextFlagSet::NOFLAGS, None)
        .map_err(|e| {
            mainloop.unlock();
            format!("PulseAudio unavailable: failed to connect: {e}")
        })?;

    loop {
        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                mainloop.unlock();
                mainloop.stop();
                return Err("PulseAudio unavailable: connection failed".to_string());
            }
            _ => {
                mainloop.wait();
            }
        }
    }

    let sources: Arc<Mutex<Vec<ShareSource>>> = Arc::new(Mutex::new(Vec::new()));
    let done: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    let sources_clone = sources.clone();
    let done_clone = done.clone();
    let ml_ref = &mut mainloop as *mut Mainloop;

    let _op = context
        .introspect()
        .get_source_info_list(move |list_result| match list_result {
            ListResult::Item(source_info) => {
                let name_str = match &source_info.name {
                    Some(name) => name.to_string(),
                    None => return,
                };
                if !name_str.ends_with(".monitor") {
                    return;
                }

                let display_name = source_info
                    .description
                    .as_ref()
                    .map(|description| description.to_string())
                    .unwrap_or_else(|| name_str.clone());

                if let Ok(mut list) = sources_clone.lock() {
                    list.push(ShareSource {
                        id: name_str,
                        name: display_name,
                        source_type: ShareSourceType::SystemAudio,
                        thumbnail: None,
                        app_name: None,
                    });
                }
            }
            ListResult::End | ListResult::Error => {
                if let Ok(mut done) = done_clone.lock() {
                    *done = true;
                }
                unsafe { (*ml_ref).signal(false) };
            }
        });

    loop {
        if let Ok(done) = done.lock() {
            if *done {
                break;
            }
        }
        mainloop.wait();
    }

    mainloop.unlock();
    context.disconnect();
    mainloop.stop();

    let result = sources
        .lock()
        .map(|sources| sources.clone())
        .map_err(|_| "PulseAudio unavailable: failed to collect sources".to_string())?;

    Ok(result)
}

/// Enumerate Linux video sources via PipeWire and audio sources via PulseAudio.
pub(super) async fn list_sources() -> Result<EnumerationResult, String> {
    let mut warnings = Vec::new();

    let video_sources = match enumerate_pipewire_sources() {
        Ok(sources) => sources,
        Err(e) => {
            if e.contains("permission")
                || e.contains("denied")
                || e.contains("not allowed")
                || e.contains("access")
            {
                warnings.push("Direct PipeWire access unavailable - use system picker".to_string());
                Vec::new()
            } else {
                return Err(e);
            }
        }
    };

    if video_sources.is_empty() && warnings.is_empty() {
        warnings.push("Direct PipeWire access unavailable - use system picker".to_string());
    }

    let audio_sources = match enumerate_pulseaudio_sources() {
        Ok(sources) => sources,
        Err(e) => {
            warnings.push(format!("Audio source enumeration unavailable: {e}"));
            Vec::new()
        }
    };

    let mut sources = video_sources;
    sources.extend(audio_sources);

    let fallback_reason = compute_fallback_reason(&sources);

    Ok(EnumerationResult {
        sources,
        warnings,
        fallback_reason,
    })
}

/// Fetch a thumbnail for a PipeWire source by capturing one frame on a helper thread.
pub(super) async fn fetch_thumbnail(source_id: &str) -> Result<Option<String>, String> {
    if source_id.contains(".monitor") {
        return Ok(None);
    }

    let node_id: u32 = source_id
        .parse()
        .map_err(|_| format!("invalid source ID for thumbnail: {source_id}"))?;

    let (tx, rx) = std::sync::mpsc::channel();

    let handle = std::thread::Builder::new()
        .name("pw-thumb".into())
        .spawn(move || {
            let result = capture_single_frame(node_id);
            let _ = tx.send(result);
        })
        .map_err(|e| format!("failed to spawn thumbnail thread: {e}"))?;

    let frame_result = match rx.recv_timeout(THUMBNAIL_TIMEOUT) {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            log::debug!("thumbnail fetch timed out for node {node_id}");
            return Ok(None);
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            log::debug!("thumbnail thread disconnected for node {node_id}");
            return Ok(None);
        }
    };

    match frame_result {
        Ok(Some((rgba, width, height))) => encode_thumbnail_jpeg(&rgba, width, height),
        Ok(None) => Ok(None),
        Err(e) => {
            log::debug!("thumbnail capture failed for node {node_id}: {e}");
            Ok(None)
        }
    }
}

/// Capture a single RGBA frame from a PipeWire node.
fn capture_single_frame(node_id: u32) -> Result<Option<(Vec<u8>, u32, u32)>, String> {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use pipewire as pw;

    ensure_pipewire_init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| format!("PipeWire main loop failed: {e}"))?;

    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| format!("PipeWire context failed: {e}"))?;

    let core = context
        .connect_rc(None)
        .map_err(|e| format!("PipeWire connect failed: {e}"))?;

    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "wavis-thumbnail",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|e| format!("PipeWire stream failed: {e}"))?;

    type CapturedFrameData = Arc<Mutex<Option<(Vec<u8>, u32, u32)>>>;
    let frame_data: CapturedFrameData = Arc::new(Mutex::new(None));
    let got_frame = Rc::new(Cell::new(false));

    let frame_data_clone = frame_data.clone();
    let got_frame_clone = got_frame.clone();
    let mainloop_weak = mainloop.downgrade();

    let _listener = stream
        .add_local_listener()
        .process(move |stream, _user_data: &mut ()| {
            if got_frame_clone.get() {
                return;
            }

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

            let raw_slice = unsafe { std::slice::from_raw_parts(data_ptr as *const u8, size) };

            let mut rgba = Vec::with_capacity(width * height * 4);
            for row in 0..height {
                let row_start = row * stride;
                for col in 0..width {
                    let px = row_start + col * bytes_per_pixel;
                    if px + 3 > raw_slice.len() {
                        break;
                    }
                    rgba.push(raw_slice[px + 2]);
                    rgba.push(raw_slice[px + 1]);
                    rgba.push(raw_slice[px]);
                    rgba.push(255);
                }
            }

            unsafe { stream.queue_raw_buffer(buffer_ptr) };

            if let Ok(mut guard) = frame_data_clone.lock() {
                *guard = Some((rgba, width as u32, height as u32));
            }

            got_frame_clone.set(true);

            if let Some(mainloop) = mainloop_weak.upgrade() {
                mainloop.quit();
            }
        })
        .register();

    let flags = pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
    stream
        .connect(
            pw::spa::utils::Direction::Input,
            Some(node_id),
            flags,
            &mut [],
        )
        .map_err(|e| format!("PipeWire stream connect failed for node {node_id}: {e}"))?;

    mainloop.run();

    let _ = stream.disconnect();

    let result = frame_data
        .lock()
        .map_err(|_| "failed to read captured frame".to_string())?
        .take();

    Ok(result)
}
