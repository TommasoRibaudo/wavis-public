//! PipeWire screen capture helper — runs out-of-process to avoid
//! PipeWire/WebKitGTK conflicts in the main Tauri process.
//!
//! Protocol:
//! - stdout: length-prefixed JPEG frames
//!   `u32 LE width | u32 LE height | u32 LE jpeg_len | jpeg bytes`
//! - stderr: human-readable log messages
//! - stdin:  when closed → helper exits gracefully
//! - exit 0: normal shutdown (stdin closed)
//! - exit 1: error (details on stderr)
//! - exit 2: user cancelled portal picker

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("pw_capture_helper is only available on Linux");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
use std::io::{Cursor, Write};
#[cfg(target_os = "linux")]
use std::os::fd::BorrowedFd;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
#[cfg(target_os = "linux")]
use ashpd::desktop::PersistMode;
#[cfg(target_os = "linux")]
use pipewire as pw;
#[cfg(target_os = "linux")]
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
#[cfg(target_os = "linux")]
use pw::spa::param::video::VideoFormat;
#[cfg(target_os = "linux")]
use pw::spa::param::ParamType;
#[cfg(target_os = "linux")]
use pw::spa::pod::serialize::PodSerializer;
#[cfg(target_os = "linux")]
use pw::spa::pod::{self, Object, Pod, Property, Value};
#[cfg(target_os = "linux")]
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id, SpaTypes};

#[cfg(target_os = "linux")]
fn main() {
    // Redirect all stderr/eprintln to a log file for diagnostic access.
    let log_path = "/tmp/pw_capture_helper.log";
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
    {
        use std::os::unix::io::IntoRawFd;
        let fd = file.into_raw_fd();
        unsafe {
            libc::dup2(fd, 2); // redirect stderr to file
            libc::close(fd);
        }
    }

    // Parse optional max fps from args (default 30).
    let requested_max_fps: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
        .max(1);
    let requested_max_width: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1920)
        .max(1);
    let requested_max_height: u32 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1080)
        .max(1);
    // Parse optional JPEG quality from args (default 85).
    let jpeg_quality: u8 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(85);

    eprintln!(
        "pw_capture_helper: starting (requested_max_fps={requested_max_fps}, requested_max={}x{}, jpeg_quality={jpeg_quality})",
        requested_max_width,
        requested_max_height
    );

    pw::init();
    eprintln!("pw_capture_helper: pipewire initialized");

    // Run portal session to get a PipeWire node + fd.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("pw_capture_helper: failed to create tokio runtime: {e}");
            std::process::exit(1);
        });

    let portal = match rt.block_on(acquire_portal_session()) {
        Ok(p) => p,
        Err(PortalError::UserCancelled) => {
            eprintln!("pw_capture_helper: user cancelled");
            std::process::exit(2);
        }
        Err(PortalError::Failed(msg)) => {
            eprintln!("pw_capture_helper: portal error: {msg}");
            std::process::exit(1);
        }
    };

    eprintln!(
        "pw_capture_helper: portal session acquired, node_id={}",
        portal.node_id
    );

    // Signal the parent process that the portal session succeeded.
    // Protocol: write "READY\n" (6 bytes) before starting frame stream.
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        if stdout.write_all(b"READY\n").is_err() || stdout.flush().is_err() {
            eprintln!("pw_capture_helper: failed to write READY signal");
            std::process::exit(1);
        }
    }

    // Monitor stdin on a separate thread — when it closes, signal quit.
    let running = Arc::new(AtomicBool::new(true));
    let running_stdin = running.clone();
    let (quit_tx, quit_rx) = pw::channel::channel::<()>();

    std::thread::Builder::new()
        .name("stdin-monitor".into())
        .spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 1];
            // Block until stdin closes (parent process closes pipe or exits).
            let _ = std::io::stdin().read(&mut buf);
            running_stdin.store(false, Ordering::SeqCst);
            let _ = quit_tx.send(());
            eprintln!("pw_capture_helper: stdin closed, shutting down");
        })
        .expect("failed to spawn stdin monitor");

    // PipeWire main loop — capture frames and write to stdout.
    run_capture(
        portal,
        running,
        quit_rx,
        jpeg_quality,
        requested_max_fps,
        requested_max_width,
        requested_max_height,
    );
}

#[cfg(target_os = "linux")]
struct PortalSession {
    node_id: u32,
    pw_fd: std::os::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
enum PortalError {
    UserCancelled,
    Failed(String),
}

#[cfg(target_os = "linux")]
async fn acquire_portal_session() -> Result<PortalSession, PortalError> {
    eprintln!("pw_capture_helper: connecting to ScreenCast portal");
    let proxy = Screencast::new()
        .await
        .map_err(|e| PortalError::Failed(format!("connect to portal: {e}")))?;

    eprintln!("pw_capture_helper: CreateSession");
    let session = proxy
        .create_session()
        .await
        .map_err(|e| PortalError::Failed(format!("create session: {e}")))?;

    eprintln!("pw_capture_helper: SelectSources");
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor | SourceType::Window,
            false,
            None,
            PersistMode::DoNot,
        )
        .await
        .map_err(|e| PortalError::Failed(format!("select sources: {e}")))?;

    eprintln!("pw_capture_helper: Start (showing picker)");
    let response = proxy
        .start(&session, None)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("cancelled") || msg.contains("Cancelled") {
                PortalError::UserCancelled
            } else {
                PortalError::Failed(format!("start: {e}"))
            }
        })?
        .response()
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("cancelled") || msg.contains("Cancelled") {
                PortalError::UserCancelled
            } else {
                PortalError::Failed(format!("response: {e}"))
            }
        })?;

    let streams = response.streams();
    if streams.is_empty() {
        return Err(PortalError::UserCancelled);
    }

    let node_id = streams[0].pipe_wire_node_id();
    eprintln!("pw_capture_helper: selected node {node_id}");

    let pw_fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(|e| PortalError::Failed(format!("open PipeWire remote: {e}")))?;

    Ok(PortalSession { node_id, pw_fd })
}

/// Build a SPA format pod requesting BGRx (preferred), with BGRA/RGBx/RGBA
/// as acceptable alternatives, and explicitly set modifier to LINEAR (0)
/// to prevent DMA-BUF tiled format negotiation.
#[cfg(target_os = "linux")]
fn build_format_pod(requested_max_fps: u32) -> Vec<u8> {
    let _ = requested_max_fps;
    let format_obj = Value::Object(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoFormat.as_raw(),
                Value::Choice(pod::ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(VideoFormat::BGRx.as_raw()),
                        alternatives: vec![
                            Id(VideoFormat::BGRx.as_raw()),
                            Id(VideoFormat::BGRA.as_raw()),
                            Id(VideoFormat::RGBx.as_raw()),
                            Id(VideoFormat::RGBA.as_raw()),
                        ],
                    },
                ))),
            ),
        ],
    });

    let (cursor, _) = PodSerializer::serialize(Cursor::new(Vec::new()), &format_obj)
        .expect("failed to serialize format pod");
    cursor.into_inner()
}

/// Build a SPA buffers param pod that restricts data types to MemPtr and MemFd,
/// excluding DmaBuf. This forces PipeWire to deliver CPU-readable shared memory
/// buffers instead of GPU DMA-BUFs.
#[cfg(target_os = "linux")]
fn build_buffers_param_pod() -> Vec<u8> {
    // SPA_PARAM_BUFFERS_dataType is property key 4 in the ParamBuffers object.
    // Value is a bitmask: MemPtr(1<<1) | MemFd(1<<2) | DmaBuf(1<<3) = 14
    // DMA-BUF is needed on Hyprland where the compositor provides GPU buffers only.
    const SPA_PARAM_BUFFERS_DATA_TYPE: u32 = 4;
    let data_type_mask: i32 = (1 << 1) | (1 << 2) | (1 << 3); // MemPtr | MemFd | DmaBuf

    let buffers_obj = Value::Object(Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![Property::new(
            SPA_PARAM_BUFFERS_DATA_TYPE,
            Value::Int(data_type_mask),
        )],
    });

    let (cursor, _) = PodSerializer::serialize(Cursor::new(Vec::new()), &buffers_obj)
        .expect("failed to serialize buffers param pod");
    cursor.into_inner()
}

/// User data passed through PipeWire stream callbacks.
/// Tracks the negotiated pixel format so process_frame knows
/// how to interpret the raw buffer.
#[cfg(target_os = "linux")]
struct CaptureState {
    /// The negotiated video format (None until param_changed fires).
    format: Option<VideoFormat>,
    /// Negotiated frame dimensions (from param_changed).
    width: u32,
    height: u32,
    /// Frame counter for periodic diagnostic logging.
    frame_count: u64,
    /// Timestamp of the last frame we actually processed and emitted.
    last_emit_at: Option<std::time::Instant>,
    /// GBM device for importing DMA-BUF buffers.
    gbm_device: Option<gbm::Device<std::fs::File>>,
}

#[cfg(target_os = "linux")]
impl Default for CaptureState {
    fn default() -> Self {
        // Open /dev/dri/renderD128 for GBM buffer import.
        let gbm = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/dri/renderD128")
        {
            Ok(file) => match gbm::Device::new(file) {
                Ok(dev) => {
                    eprintln!("pw_capture_helper: GBM device opened on /dev/dri/renderD128");
                    Some(dev)
                }
                Err(e) => {
                    eprintln!("pw_capture_helper: GBM device creation failed: {e}");
                    None
                }
            },
            Err(e) => {
                eprintln!("pw_capture_helper: failed to open /dev/dri/renderD128: {e}");
                None
            }
        };

        Self {
            format: None,
            width: 0,
            height: 0,
            frame_count: 0,
            last_emit_at: None,
            gbm_device: gbm,
        }
    }
}

#[cfg(target_os = "linux")]
fn run_capture(
    portal: PortalSession,
    running: Arc<AtomicBool>,
    quit_rx: pw::channel::Receiver<()>,
    jpeg_quality: u8,
    requested_max_fps: u32,
    requested_max_width: u32,
    requested_max_height: u32,
) {
    let mainloop = pw::main_loop::MainLoopRc::new(None).unwrap_or_else(|e| {
        eprintln!("pw_capture_helper: MainLoopRc::new failed: {e}");
        std::process::exit(1);
    });
    eprintln!("pw_capture_helper: MainLoop created");

    let context = pw::context::ContextRc::new(&mainloop, None).unwrap_or_else(|e| {
        eprintln!("pw_capture_helper: ContextRc::new failed: {e}");
        std::process::exit(1);
    });

    let core = context
        .connect_fd_rc(portal.pw_fd, None)
        .unwrap_or_else(|e| {
            eprintln!("pw_capture_helper: connect_fd failed: {e}");
            std::process::exit(1);
        });
    eprintln!("pw_capture_helper: connected to PipeWire via portal fd");

    // Attach quit receiver.
    let mainloop_weak = mainloop.downgrade();
    let _quit_listener = quit_rx.attach(mainloop.loop_(), move |_| {
        if let Some(ml) = mainloop_weak.upgrade() {
            ml.quit();
        }
    });

    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "wavis-screen-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .unwrap_or_else(|e| {
        eprintln!("pw_capture_helper: Stream::new failed: {e}");
        std::process::exit(1);
    });

    let running_cb = running.clone();
    let _listener = stream
        .add_local_listener::<CaptureState>()
        .param_changed(|_stream, state, id, param| {
            eprintln!(
                "pw_capture_helper: param_changed id={} (Format={})",
                id,
                ParamType::Format.as_raw()
            );
            // Only handle Format parameter changes.
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                eprintln!("pw_capture_helper: param_changed: param is None");
                return;
            };

            let mut info = pw::spa::param::video::VideoInfoRaw::new();
            match info.parse(param) {
                Ok(_) => {
                    let fmt = info.format();
                    let size = info.size();
                    eprintln!(
                        "pw_capture_helper: FORMAT NEGOTIATED: {:?} ({}x{}, framerate={}/{})",
                        fmt,
                        size.width,
                        size.height,
                        info.framerate().num,
                        info.framerate().denom,
                    );
                    state.format = Some(fmt);
                    state.width = size.width;
                    state.height = size.height;
                }
                Err(e) => {
                    eprintln!("pw_capture_helper: failed to parse video info from param: {e}");
                }
            }
        })
        .process(move |stream, state: &mut CaptureState| {
            if !running_cb.load(Ordering::SeqCst) {
                return;
            }

            let min_frame_interval =
                std::time::Duration::from_nanos(1_000_000_000 / requested_max_fps.max(1) as u64);
            let now = std::time::Instant::now();
            if let Some(last_emit_at) = state.last_emit_at {
                if now.duration_since(last_emit_at) < min_frame_interval {
                    return;
                }
            }
            state.last_emit_at = Some(now);
            state.frame_count += 1;

            if state.frame_count <= 3 || state.frame_count.is_multiple_of(300) {
                eprintln!(
                    "pw_capture_helper: process frame #{}, format={:?}",
                    state.frame_count, state.format
                );
            }

            let swap_rb = match state.format {
                // BGRx/BGRA: bytes are [B,G,R,x/A] — need to swap R↔B
                Some(VideoFormat::BGRx) | Some(VideoFormat::BGRA) => true,
                // RGBx/RGBA: bytes are [R,G,B,x/A] — already correct order
                Some(VideoFormat::RGBx) | Some(VideoFormat::RGBA) => false,
                None => {
                    eprintln!(
                        "pw_capture_helper: WARNING frame #{} arrived before format negotiation, assuming BGRx",
                        state.frame_count
                    );
                    true
                }
                Some(other) => {
                    eprintln!("pw_capture_helper: unsupported format {:?}, skipping frame #{}", other, state.frame_count);
                    return;
                }
            };
            process_frame(
                stream,
                jpeg_quality,
                swap_rb,
                state.frame_count,
                state.width,
                state.height,
                requested_max_width as usize,
                requested_max_height as usize,
                &state.gbm_device,
            );
        })
        .register();

    // Build format parameters requesting BGRx/BGRA/RGBx/RGBA.
    let format_bytes = build_format_pod(requested_max_fps);
    let format_pod = Pod::from_bytes(&format_bytes).expect("failed to parse format pod");

    // Build buffers param allowing SHM (MemPtr|MemFd) and DMA-BUF.
    let buffers_bytes = build_buffers_param_pod();
    let _buffers_pod = Pod::from_bytes(&buffers_bytes).expect("failed to parse buffers param pod");

    // No ALLOC_BUFFERS, no buffers param: let PipeWire decide buffer allocation entirely.
    // MAP_BUFFERS: maps provided buffers into our address space.
    // On Hyprland this typically yields MemFd buffers which we mmap manually.
    let flags = pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
    eprintln!(
        "pw_capture_helper: connecting stream with MAP_BUFFERS only (compositor decides buffers)"
    );
    if let Err(e) = stream.connect(
        pw::spa::utils::Direction::Input,
        Some(portal.node_id),
        flags,
        &mut [format_pod],
    ) {
        eprintln!("pw_capture_helper: stream connect failed: {e}");
        std::process::exit(1);
    }
    eprintln!("pw_capture_helper: stream connected with format negotiation, entering main loop");

    mainloop.run();

    let _ = stream.disconnect();
    eprintln!("pw_capture_helper: exiting");
}

#[cfg(target_os = "linux")]
fn process_frame(
    stream: &pw::stream::Stream,
    jpeg_quality: u8,
    swap_rb: bool,
    frame_num: u64,
    nego_w: u32,
    nego_h: u32,
    requested_max_width: usize,
    requested_max_height: usize,
    gbm_device: &Option<gbm::Device<std::fs::File>>,
) {
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

    let width = nego_w as usize;
    let height = nego_h as usize;
    let bytes_per_pixel = 4usize;

    if width == 0 || height == 0 {
        unsafe { stream.queue_raw_buffer(buffer_ptr) };
        return;
    }

    let chunk = unsafe { &*data_ref.chunk };
    let chunk_size = chunk.size as usize;
    let stride = chunk.stride as usize;

    if frame_num <= 3 {
        eprintln!(
            "pw_capture_helper: frame #{frame_num}: type={} fd={} data_null={} chunk_size={chunk_size} stride={stride} maxsize={}",
            data_ref.type_, data_ref.fd, data_ptr.is_null(), data_ref.maxsize,
        );
    }

    // SPA_DATA_DmaBuf = 3. Use GBM to import and map the DMA-BUF for linear CPU reads.
    // SPA_DATA_MemFd=2, SPA_DATA_DmaBuf=3. Both can carry GPU-backed buffers
    // MemFd buffers are CPU-readable shared memory (mapped by MAP_BUFFERS).
    // Only DMA-BUF (type=3) needs GBM import for CPU access.
    const SPA_DATA_MEMFD: u32 = 2;
    const SPA_DATA_DMABUF: u32 = 3;
    let has_fd = data_ref.fd >= 0;
    let is_gpu_buffer = data_ref.type_ == SPA_DATA_DMABUF && has_fd;

    if frame_num <= 3 {
        eprintln!(
            "pw_capture_helper: frame #{frame_num}: is_gpu_buffer={is_gpu_buffer} type={} fd={} has_fd={has_fd}",
            data_ref.type_, data_ref.fd,
        );
    }

    let rgb = if is_gpu_buffer {
        let Some(gbm) = gbm_device else {
            eprintln!("pw_capture_helper: frame #{frame_num}: DMA-BUF but no GBM device");
            unsafe { stream.queue_raw_buffer(buffer_ptr) };
            return;
        };

        let fd = data_ref.fd;
        if fd < 0 {
            unsafe { stream.queue_raw_buffer(buffer_ptr) };
            return;
        }

        let dmabuf_fd = unsafe { BorrowedFd::borrow_raw(fd as i32) };

        // Import the DMA-BUF into GBM.
        // GBM_FORMAT_ARGB8888 = little-endian ARGB = memory order B,G,R,A (same as BGRA).
        let drm_format = if swap_rb {
            gbm::Format::Argb8888
        } else {
            gbm::Format::Abgr8888
        };

        let import_stride = if stride > 0 {
            stride as u32
        } else {
            (width * bytes_per_pixel) as u32
        };

        let bo = match gbm.import_buffer_object_from_dma_buf::<()>(
            dmabuf_fd,
            width as u32,
            height as u32,
            import_stride,
            drm_format,
            gbm::BufferObjectFlags::empty(),
        ) {
            Ok(bo) => bo,
            Err(e) => {
                if frame_num <= 3 {
                    eprintln!("pw_capture_helper: frame #{frame_num}: GBM import failed: {e}");
                }
                unsafe { stream.queue_raw_buffer(buffer_ptr) };
                return;
            }
        };

        // Map the buffer for CPU read — GBM handles detiling internally.
        let result = bo.map(0, 0, width as u32, height as u32, |mapping| {
            let mapped_data = mapping.buffer();
            let map_stride = mapping.stride() as usize;

            if frame_num <= 3 {
                eprintln!(
                    "pw_capture_helper: frame #{frame_num}: GBM mapped OK, len={} map_stride={}",
                    mapped_data.len(),
                    map_stride,
                );
                if mapped_data.len() >= 4 {
                    eprintln!(
                        "pw_capture_helper: frame #{frame_num}: GBM first_pixel=[{},{},{},{}]",
                        mapped_data[0], mapped_data[1], mapped_data[2], mapped_data[3]
                    );
                }
            }

            // Convert BGRA/RGBA → RGB for JPEG encoding.
            let mut rgb = Vec::with_capacity(width * height * 3);
            for row in 0..height {
                let row_start = row * map_stride;
                for col in 0..width {
                    let px = row_start + col * bytes_per_pixel;
                    if px + 3 > mapped_data.len() {
                        break;
                    }
                    if swap_rb {
                        rgb.push(mapped_data[px + 2]); // R
                        rgb.push(mapped_data[px + 1]); // G
                        rgb.push(mapped_data[px]); // B
                    } else {
                        rgb.push(mapped_data[px]); // R
                        rgb.push(mapped_data[px + 1]); // G
                        rgb.push(mapped_data[px + 2]); // B
                    }
                }
            }

            // Debug: dump first frame as PNG.
            if frame_num == 1 {
                if let Some(img) =
                    image::RgbImage::from_raw(width as u32, height as u32, rgb.clone())
                {
                    let _ = img.save("/tmp/pw_debug_converted.png");
                    eprintln!("pw_capture_helper: DEBUG wrote /tmp/pw_debug_converted.png");
                }
            }

            rgb
        });

        // Return PipeWire buffer after GBM map/unmap is done.
        unsafe { stream.queue_raw_buffer(buffer_ptr) };

        match result {
            Ok(rgb) => rgb,
            Err(e) => {
                if frame_num <= 3 {
                    eprintln!("pw_capture_helper: frame #{frame_num}: GBM map failed: {e}");
                }
                return;
            }
        }
    } else {
        // Non-DMA-BUF buffer (MemPtr/MemFd) — read linearly.
        let actual_stride = if stride > 0 {
            stride
        } else {
            width * bytes_per_pixel
        };
        let readable = std::cmp::min(chunk_size, data_ref.maxsize as usize);

        // For MemFd (type=2), MAP_BUFFERS may not actually map the memory.
        // Manually mmap the fd to get a readable pointer.
        let mmap_ptr: *mut libc::c_void;
        let needs_munmap;
        if data_ref.type_ == SPA_DATA_MEMFD && has_fd && data_ref.fd >= 0 {
            mmap_ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    readable,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    data_ref.fd as i32,
                    0,
                )
            };
            if mmap_ptr == libc::MAP_FAILED {
                eprintln!(
                    "pw_capture_helper: frame #{frame_num}: mmap failed for MemFd fd={}",
                    data_ref.fd
                );
                unsafe { stream.queue_raw_buffer(buffer_ptr) };
                return;
            }
            needs_munmap = true;
            if frame_num <= 3 {
                eprintln!(
                    "pw_capture_helper: frame #{frame_num}: MemFd mmap path, fd={} readable={readable}",
                    data_ref.fd,
                );
            }
        } else if !data_ptr.is_null() {
            mmap_ptr = data_ptr;
            needs_munmap = false;
            if frame_num <= 3 {
                eprintln!(
                    "pw_capture_helper: frame #{frame_num}: MemPtr path, data_ptr={:?} readable={readable}",
                    data_ptr,
                );
            }
        } else {
            unsafe { stream.queue_raw_buffer(buffer_ptr) };
            return;
        }

        let raw_slice = unsafe { std::slice::from_raw_parts(mmap_ptr as *const u8, readable) };

        if frame_num <= 3 {
            use std::io::Write;
            let _ = std::io::stderr().flush();
            if readable >= 4 {
                eprintln!(
                    "pw_capture_helper: frame #{frame_num}: first_pixel=[{},{},{},{}]",
                    raw_slice[0], raw_slice[1], raw_slice[2], raw_slice[3],
                );
                let _ = std::io::stderr().flush();
            }
        }

        let mut rgb = Vec::with_capacity(width * height * 3);
        for row in 0..height {
            let row_start = row * actual_stride;
            for col in 0..width {
                let px = row_start + col * bytes_per_pixel;
                if px + 3 > raw_slice.len() {
                    break;
                }
                if swap_rb {
                    rgb.push(raw_slice[px + 2]);
                    rgb.push(raw_slice[px + 1]);
                    rgb.push(raw_slice[px]);
                } else {
                    rgb.push(raw_slice[px]);
                    rgb.push(raw_slice[px + 1]);
                    rgb.push(raw_slice[px + 2]);
                }
            }
        }

        // Debug: dump first frame as PNG (SHM path).
        if frame_num == 1 {
            if let Some(img) = image::RgbImage::from_raw(width as u32, height as u32, rgb.clone()) {
                let _ = img.save("/tmp/pw_debug_converted_shm.png");
                eprintln!("pw_capture_helper: DEBUG wrote /tmp/pw_debug_converted_shm.png");
            }
        }

        if needs_munmap {
            unsafe { libc::munmap(mmap_ptr, readable) };
        }
        unsafe { stream.queue_raw_buffer(buffer_ptr) };
        rgb
    };

    // Downscale to max 2560×1440 before JPEG encoding to reduce CPU and pipe bandwidth.
    // Uses bilinear interpolation for readable text quality.
    let max_w = requested_max_width.max(1);
    let max_h = requested_max_height.max(1);
    let (out_rgb, out_w, out_h) = if width > max_w || height > max_h {
        let scale = f64::min(max_w as f64 / width as f64, max_h as f64 / height as f64);
        let new_w = ((width as f64 * scale) as usize).max(1);
        let new_h = ((height as f64 * scale) as usize).max(1);
        let mut scaled = vec![0u8; new_w * new_h * 3];
        for row in 0..new_h {
            let src_yf = row as f64 / scale;
            let sy0 = (src_yf as usize).min(height.saturating_sub(2));
            let sy1 = sy0 + 1;
            let fy = src_yf - sy0 as f64;
            for col in 0..new_w {
                let src_xf = col as f64 / scale;
                let sx0 = (src_xf as usize).min(width.saturating_sub(2));
                let sx1 = sx0 + 1;
                let fx = src_xf - sx0 as f64;
                let dst_idx = (row * new_w + col) * 3;
                for c in 0..3 {
                    let p00 = rgb[(sy0 * width + sx0) * 3 + c] as f64;
                    let p10 = rgb[(sy0 * width + sx1) * 3 + c] as f64;
                    let p01 = rgb[(sy1 * width + sx0) * 3 + c] as f64;
                    let p11 = rgb[(sy1 * width + sx1) * 3 + c] as f64;
                    let val = p00 * (1.0 - fx) * (1.0 - fy)
                        + p10 * fx * (1.0 - fy)
                        + p01 * (1.0 - fx) * fy
                        + p11 * fx * fy;
                    scaled[dst_idx + c] = val.clamp(0.0, 255.0) as u8;
                }
            }
        }
        if frame_num <= 3 {
            eprintln!(
                "pw_capture_helper: frame #{frame_num}: downscaled {width}x{height} → {new_w}x{new_h} (bilinear)",
            );
        }
        (scaled, new_w, new_h)
    } else {
        (rgb, width, height)
    };

    // JPEG encode.
    let mut jpeg_buf = Vec::with_capacity(out_w * out_h / 4);
    {
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, jpeg_quality);
        if let Err(e) = encoder.encode(
            &out_rgb,
            out_w as u32,
            out_h as u32,
            image::ExtendedColorType::Rgb8,
        ) {
            eprintln!("pw_capture_helper: JPEG encode failed: {e}");
            return;
        }
    }

    // Write frame header + JPEG data to stdout.
    let w = out_w as u32;
    let h = out_h as u32;
    let len = jpeg_buf.len() as u32;

    if frame_num <= 3 || frame_num.is_multiple_of(300) {
        eprintln!("pw_capture_helper: frame #{frame_num}: sending {w}x{h} jpeg_len={len} bytes",);
    }

    let mut stdout = std::io::stdout().lock();
    let ok = stdout.write_all(&w.to_le_bytes()).is_ok()
        && stdout.write_all(&h.to_le_bytes()).is_ok()
        && stdout.write_all(&len.to_le_bytes()).is_ok()
        && stdout.write_all(&jpeg_buf).is_ok()
        && stdout.flush().is_ok();

    if !ok {
        // Stdout closed (parent died) — exit.
        std::process::exit(0);
    }
}
