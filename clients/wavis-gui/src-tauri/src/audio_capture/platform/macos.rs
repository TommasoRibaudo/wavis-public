//! macOS audio capture via ScreenCaptureKit, Core Audio process taps, and the
//! virtual-device isolation fallback.
//!
//! macOS 14.2+: AudioHardwareCreateProcessTap excludes Wavis main PID at the
//! kernel audio graph level, including WKWebView helper processes.
//! If the tap path fails at runtime, we try virtual-device routing before
//! falling back to ScreenCaptureKit so screen-share audio still starts instead
//! of hard-failing.
//! ScreenCaptureKit is the baseline path on macOS 12.3+ and uses content
//! filters plus `excludesCurrentProcessAudio`, but it does not provide routing
//! isolation when used as the final fallback.
//!
//! Both paths emit wasapi_audio_frame events with base64-encoded 960-sample
//! i16 PCM (20ms @ 48kHz mono) - identical to the Windows event format.
//!
//! FFI safety: see individual extern blocks and unsafe impl Send on TapCaptureHandle.

use tauri::{AppHandle, State};

use super::super::audio_capture_state::{
    AudioCaptureState, AudioShareStartResult, IoProcCtx, MacAudioHandle, ScCaptureHandle,
    TapCaptureHandle, VirtualDeviceCaptureHandle, VirtualDeviceRoutingState,
};
use super::macos_routing::{
    bare_sck_fallback_result, plan_virtual_device_teardown, select_macos_audio_share_decision,
    MacAudioShareDecision, VirtualDeviceTeardownAction, VirtualDeviceTeardownSnapshot,
    VirtualDeviceTeardownTrigger,
};
use super::macos_virtual_device::{
    cleanup_stale_multi_output_devices, create_multi_output_device, destroy_multi_output_device,
    detect_virtual_audio_device, find_hardware_speaker_uid_in_aggregate,
    get_real_output_device_uid, restore_system_default_output, swap_system_default_output,
};
use super::{try_start_process_tap, MacOsVersion};

macro_rules! share_audio_info {
    ($($arg:tt)*) => {
        if crate::debug_env::debug_share_audio_enabled() {
            log::info!($($arg)*);
        }
    };
}

pub(super) fn resolve_monitor() -> Result<String, String> {
    Ok("system".to_string())
}

pub(super) fn resolve_monitor_fast() -> Result<String, String> {
    resolve_monitor()
}

pub(super) fn current_macos_version() -> MacOsVersion {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSOperatingSystemVersion {
        major_version: isize,
        minor_version: isize,
        patch_version: isize,
    }
    unsafe impl objc2::Encode for NSOperatingSystemVersion {
        const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
            "NSOperatingSystemVersion",
            &[isize::ENCODING, isize::ENCODING, isize::ENCODING],
        );
    }
    unsafe impl objc2::RefEncode for NSOperatingSystemVersion {
        const ENCODING_REF: objc2::Encoding =
            objc2::Encoding::Pointer(&<Self as objc2::Encode>::ENCODING);
    }
    let raw: NSOperatingSystemVersion = unsafe {
        let info: &objc2_foundation::NSProcessInfo =
            objc2::msg_send![objc2_foundation::NSProcessInfo::class(), processInfo];
        objc2::msg_send![info, operatingSystemVersion]
    };
    MacOsVersion {
        major: raw.major_version,
        minor: raw.minor_version,
        patch: raw.patch_version,
    }
}

pub(super) fn start(
    source_id: String,
    state: State<'_, crate::media::MediaState>,
    audio_capture: State<'_, AudioCaptureState>,
    app: AppHandle,
) -> Result<AudioShareStartResult, String> {
    audio_share_start_macos(source_id, state, audio_capture, app)
}

pub(super) fn stop(
    state: State<'_, crate::media::MediaState>,
    audio_capture: State<'_, AudioCaptureState>,
) -> Result<(), String> {
    audio_share_stop_macos(state, audio_capture)
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;

    base64::engine::general_purpose::STANDARD.encode(input)
}

// ─── macOS Audio Capture (ScreenCaptureKit) ────────────────────────────────
//
// Requires macOS 12.3+. Captures system audio via SCStream, excludes Wavis
// from the loopback using a content filter (all macOS versions) and
// `excludesCurrentProcessAudio` (macOS 13+ belt-and-suspenders).
//
// Frames are forwarded to the JS frontend via the same "wasapi_audio_frame"
// Tauri event used by Windows WASAPI — the JS AudioWorklet bridge is
// platform-agnostic.

// ── CoreMedia C API — audio buffer extraction ─────────────────────────────
//
// We use the CoreMedia C API directly rather than relying on the exact Rust
// API shape of objc2-core-media, giving us a stable interface independent of
// binding generation details.

#[cfg(target_os = "macos")]
#[repr(C)]
struct MacAudioBuffer {
    mNumberChannels: u32,
    mDataByteSize: u32,
    mData: *mut std::ffi::c_void,
}

/// Stack-allocated AudioBufferList with room for up to 2 channels.
/// ScreenCaptureKit typically delivers mono (channelCount=1) or interleaved
/// stereo; 2 buffers covers both cases.
#[cfg(target_os = "macos")]
#[repr(C)]
struct MacAudioBufferList {
    mNumberBuffers: u32,
    mBuffers: [MacAudioBuffer; 2],
}

#[cfg(target_os = "macos")]
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferDataIsReady(sbuf: *const std::ffi::c_void) -> bool;
    fn CMSampleBufferMakeDataReady(sbuf: *mut std::ffi::c_void) -> i32; // OSStatus
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sbuf: *const std::ffi::c_void,
        buffer_list_size_needed_out: *mut usize,
        buffer_list: *mut MacAudioBufferList,
        buffer_list_size: usize,
        block_buffer_structure_allocator: *const std::ffi::c_void,
        block_buffer_block_allocator: *const std::ffi::c_void,
        flags: u32,
        block_buffer_out: *mut *mut std::ffi::c_void,
    ) -> i32; // OSStatus — 0 on success
    fn CMSampleBufferGetNumSamples(sbuf: *const std::ffi::c_void) -> i64; // CMItemCount
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
    fn CFStringCreateWithCString(
        alloc: *const std::ffi::c_void, // pass NULL for default allocator
        c_str: *const std::ffi::c_char,
        encoding: u32, // kCFStringEncodingUTF8
    ) -> *const std::ffi::c_void; // CFStringRef
}

// ── AudioQueue input FFI (AudioToolbox) ───────────────────────────────────
//
// Used by the virtual-device capture path to read from the loopback device's
// input stream without registering an IOProc on the aggregate output device.
// AudioQueueNewInput does not put any device in "pull mode", so the stacked
// multi-output aggregate can route audio passively to speakers + BlackHole.

/// Opaque `AudioQueueBuffer` — only `mAudioData` and `mAudioDataByteSize` are
/// accessed in the callback; the rest is managed internally by the HAL.
#[cfg(target_os = "macos")]
#[repr(C)]
pub(crate) struct AudioQueueBuffer {
    pub(crate) m_audio_data_bytes_capacity: u32,
    pub(crate) m_audio_data: *mut std::ffi::c_void,
    pub(crate) m_audio_data_byte_size: u32,
    pub(crate) m_user_data: *mut std::ffi::c_void,
    pub(crate) m_packet_description_capacity: u32,
    pub(crate) m_packet_descriptions: *mut std::ffi::c_void,
    pub(crate) m_packet_description_count: u32,
}

#[cfg(target_os = "macos")]
#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioQueueNewInput(
        in_format: *const TapStreamBasicDescription,
        in_callback: unsafe extern "C" fn(
            *mut std::ffi::c_void,   // inUserData
            *mut std::ffi::c_void,   // AudioQueueRef
            *mut AudioQueueBuffer,   // AudioQueueBufferRef
            *const std::ffi::c_void, // AudioTimeStamp*
            u32,                     // inNumberPacketDescriptions
            *const std::ffi::c_void, // AudioStreamPacketDescription*
        ),
        in_user_data: *mut std::ffi::c_void,
        in_callback_run_loop: *const std::ffi::c_void, // NULL → internal thread
        in_callback_run_loop_mode: *const std::ffi::c_void, // NULL
        in_flags: u32,
        out_aq: *mut *mut std::ffi::c_void,
    ) -> i32;

    fn AudioQueueSetProperty(
        in_aq: *mut std::ffi::c_void,
        in_id: u32,
        in_data: *const std::ffi::c_void,
        in_data_size: u32,
    ) -> i32;

    fn AudioQueueAllocateBuffer(
        in_aq: *mut std::ffi::c_void,
        in_buffer_byte_size: u32,
        out_buffer: *mut *mut AudioQueueBuffer,
    ) -> i32;

    fn AudioQueueEnqueueBuffer(
        in_aq: *mut std::ffi::c_void,
        in_buffer: *mut AudioQueueBuffer,
        in_num_packet_descs: u32,
        in_packet_descs: *const std::ffi::c_void,
    ) -> i32;

    fn AudioQueueStart(
        in_aq: *mut std::ffi::c_void,
        in_start_time: *const std::ffi::c_void, // NULL = start immediately
    ) -> i32;

    fn AudioQueueStop(
        in_aq: *mut std::ffi::c_void,
        in_immediate: u8, // Boolean: 1 = synchronous stop
    ) -> i32;

    fn AudioQueueDispose(
        in_aq: *mut std::ffi::c_void,
        in_immediate: u8, // Boolean: 1 = synchronous dispose
    ) -> i32;
}

// kAudioQueueProperty_CurrentDevice = 'aqcd'
#[cfg(target_os = "macos")]
const K_AUDIO_QUEUE_PROPERTY_CURRENT_DEVICE: u32 = 0x61716364;
// kCFStringEncodingUTF8 = 0x08000100
#[cfg(target_os = "macos")]
const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
// kAudioFormatLinearPCM = 'lpcm'
#[cfg(target_os = "macos")]
const K_AUDIO_FORMAT_LINEAR_PCM: u32 = 0x6C70636D;
// kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked (interleaved float32 PCM)
#[cfg(target_os = "macos")]
const K_AUDIO_FORMAT_FLAGS_FLOAT_PACKED: u32 = 0x09;

// ── Core Audio process tap FFI (macOS 14.2+) ─────────────────────────────
//
// AudioHardwareCreateProcessTap and CATapDescription were added in macOS 14.2.
// All other functions (AudioDeviceCreateIOProcID etc.) are standard CoreAudio
// C APIs available since macOS 10.x.

/// `AudioObjectPropertyAddress` — used to query the tap's stream format.
#[cfg(target_os = "macos")]
#[repr(C)]
struct TapPropertyAddress {
    m_selector: u32,
    m_scope: u32,
    m_element: u32,
}

/// `AudioStreamBasicDescription` — describes the audio format of a stream.
#[cfg(target_os = "macos")]
#[repr(C)]
struct TapStreamBasicDescription {
    m_sample_rate: f64,
    m_format_id: u32,
    m_format_flags: u32,
    m_bytes_per_packet: u32,
    m_frames_per_packet: u32,
    m_bytes_per_frame: u32,
    m_channels_per_frame: u32,
    m_bits_per_channel: u32,
    m_reserved: u32,
}

#[cfg(target_os = "macos")]
#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    /// Create a process tap from a `CATapDescription` (macOS 14.2+).
    fn AudioHardwareCreateProcessTap(
        in_description: *mut std::ffi::c_void, // CATapDescription*
        out_tap_id: *mut u32,                  // AudioObjectID*
    ) -> i32; // OSStatus

    /// Destroy a previously created process tap.
    fn AudioHardwareDestroyProcessTap(in_tap_id: u32) -> i32;

    /// Register an IOProc callback on a CoreAudio device (or tap object).
    fn AudioDeviceCreateIOProcID(
        in_device: u32,
        in_proc: unsafe extern "C" fn(
            u32,
            *const std::ffi::c_void,
            *const MacAudioBufferList,
            *const std::ffi::c_void,
            *mut MacAudioBufferList,
            *const std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> i32,
        in_client_data: *mut std::ffi::c_void,
        out_ioproc_id: *mut *mut std::ffi::c_void,
    ) -> i32;

    /// Unregister an IOProc.
    fn AudioDeviceDestroyIOProcID(in_device: u32, in_ioproc_id: *mut std::ffi::c_void) -> i32;

    /// Start the IOProc on a device.
    fn AudioDeviceStart(in_device: u32, in_ioproc_id: *mut std::ffi::c_void) -> i32;

    /// Stop the IOProc on a device.
    fn AudioDeviceStop(in_device: u32, in_ioproc_id: *mut std::ffi::c_void) -> i32;

    /// Read a property from a CoreAudio object (device, tap, etc.).
    fn AudioObjectGetPropertyData(
        in_object_id: u32,
        in_address: *const TapPropertyAddress,
        in_qualifier_data_size: u32,
        in_qualifier_data: *const std::ffi::c_void,
        io_data_size: *mut u32,
        out_data: *mut std::ffi::c_void,
    ) -> i32;
}

// kAudioDevicePropertyStreamFormat = 'sfmt'
#[cfg(target_os = "macos")]
const K_AUDIO_DEVICE_PROPERTY_STREAM_FORMAT: u32 = 0x73666D74;
// kAudioObjectPropertyScopeInput = 'inpt'
#[cfg(target_os = "macos")]
const K_AUDIO_OBJECT_PROPERTY_SCOPE_INPUT: u32 = 0x696E7074;
// kAudioObjectPropertyElementMain = 0
#[cfg(target_os = "macos")]
const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;
// kAudioHardwareNoError = 0
#[cfg(target_os = "macos")]
const K_AUDIO_HARDWARE_NO_ERROR: i32 = 0;

// proc_listallpids / proc_pidpath — libproc (always available on macOS,
// no entitlement required).
#[cfg(target_os = "macos")]
extern "C" {
    fn proc_listallpids(buf: *mut std::ffi::c_void, bufsize: i32) -> i32;
    fn proc_pidpath(pid: i32, buf: *mut std::ffi::c_void, bufsize: u32) -> i32;
}

#[cfg(target_os = "macos")]
const PROC_PIDPATHINFO_MAXSIZE: u32 = 4096;

#[cfg(target_os = "macos")]
fn normalize_process_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(target_os = "macos")]
fn find_app_bundle_root(path: &std::path::Path) -> Option<std::path::PathBuf> {
    path.ancestors()
        .find(|ancestor| {
            ancestor
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        })
        .map(std::path::Path::to_path_buf)
}

#[cfg(target_os = "macos")]
fn list_all_pids() -> Vec<i32> {
    let pid_count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if pid_count <= 0 {
        log::warn!("[audio_capture] tap: proc_listallpids(size probe) returned {pid_count}");
        return Vec::new();
    }

    let mut pids = vec![0i32; pid_count as usize];
    let written = unsafe {
        proc_listallpids(
            pids.as_mut_ptr() as *mut std::ffi::c_void,
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if written <= 0 {
        log::warn!("[audio_capture] tap: proc_listallpids(fill) returned {written}");
        return Vec::new();
    }

    pids.truncate(written as usize);
    pids.into_iter().filter(|pid| *pid > 0).collect()
}

#[cfg(target_os = "macos")]
fn pid_executable_path(pid: i32) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE as usize];
    let written = unsafe {
        proc_pidpath(
            pid,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            PROC_PIDPATHINFO_MAXSIZE,
        )
    };
    if written <= 0 {
        return None;
    }

    let nul_pos = buf
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(written as usize)
        .min(written as usize);
    if nul_pos == 0 {
        return None;
    }

    Some(std::path::PathBuf::from(
        String::from_utf8_lossy(&buf[..nul_pos]).into_owned(),
    ))
}

#[cfg(target_os = "macos")]
fn is_wavis_related_executable(
    pid_path: &std::path::Path,
    current_exe: &std::path::Path,
    bundle_root: Option<&std::path::Path>,
    current_dir: &std::path::Path,
    app_name_key: &str,
) -> Option<&'static str> {
    if pid_path == current_exe {
        return Some("main-exe");
    }

    if bundle_root.is_some_and(|root| pid_path.starts_with(root)) {
        return Some("same-bundle");
    }

    if app_name_key.is_empty() {
        return None;
    }

    let path_key = normalize_process_name(&pid_path.to_string_lossy());
    let same_dir = pid_path
        .parent()
        .is_some_and(|parent| parent == current_dir);
    let helperish = path_key.contains("helper")
        || path_key.contains("webkit")
        || path_key.contains("webcontent")
        || path_key.contains("networkprocess")
        || path_key.contains("gpuprocess");

    if same_dir && path_key.contains(app_name_key) {
        return Some("same-dir-name-match");
    }

    if helperish && path_key.contains(app_name_key) {
        return Some("helper-name-match");
    }

    None
}

#[cfg(target_os = "macos")]
const KCM_SAMPLE_BUFFER_ERROR_BUFFER_NOT_READY: i32 = -12733;
#[cfg(target_os = "macos")]
const KCM_SAMPLE_BUFFER_ERROR_ARRAY_TOO_SMALL: i32 = -12737;

#[cfg(target_os = "macos")]
fn sck_is_transient_audio_buffer_status(status: i32) -> bool {
    status == KCM_SAMPLE_BUFFER_ERROR_BUFFER_NOT_READY
        || status == KCM_SAMPLE_BUFFER_ERROR_ARRAY_TOO_SMALL
}

// ── AudioOutputHandler ObjC delegate ─────────────────────────────────────

#[cfg(target_os = "macos")]
struct AudioOutputIvars {
    app_handle: tauri::AppHandle,
    /// Partial-frame accumulation buffer.  Guarded by Mutex because the
    /// SCKit dispatch queue and potential stop paths run on different threads.
    accum: std::sync::Mutex<Vec<i16>>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(target_os = "macos")]
use objc2_foundation::NSObjectProtocol;
#[cfg(target_os = "macos")]
use objc2_screen_capture_kit::SCStreamOutput;

#[cfg(target_os = "macos")]
objc2::define_class!(
    /// Objective-C class that implements `SCStreamOutput` to receive audio
    /// `CMSampleBuffer` callbacks from ScreenCaptureKit on a dispatch queue.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[ivars = AudioOutputIvars]
    struct AudioOutputHandler;

    unsafe impl NSObjectProtocol for AudioOutputHandler {}

    unsafe impl SCStreamOutput for AudioOutputHandler {
        /// Called on the SCKit dispatch queue for each captured sample buffer.
        /// Only audio frames are forwarded; video frames (if any) are dropped.
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output_sample_buffer(
            &self,
            _stream: &objc2_screen_capture_kit::SCStream,
            sample_buffer: &objc2_core_media::CMSampleBuffer,
            output_type: objc2_screen_capture_kit::SCStreamOutputType,
        ) {
            use objc2::DefinedClass;
            use objc2_screen_capture_kit::SCStreamOutputType;
            use std::sync::atomic::{AtomicUsize, Ordering};

            // Diagnostic counter — print to stderr (always visible in terminal)
            static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
            let count = CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
            if count < 5 || count % 500 == 0 {
                eprintln!(
                    "[wavis-diag] sck delegate callback #{} — output_type: {:?}",
                    count + 1,
                    output_type
                );
            }

            if output_type != SCStreamOutputType::Audio {
                return;
            }
            if self.ivars().stop_flag.load(Ordering::Relaxed) {
                return;
            }
            sck_process_audio_buffer(sample_buffer, &self.ivars().app_handle, &self.ivars().accum);
        }
    }
);

#[cfg(target_os = "macos")]
impl AudioOutputHandler {
    fn new(
        app_handle: tauri::AppHandle,
        stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> objc2::rc::Retained<Self> {
        use objc2::AnyThread;
        let this = Self::alloc().set_ivars(AudioOutputIvars {
            app_handle,
            accum: std::sync::Mutex::new(Vec::with_capacity(960 * 2)),
            stop_flag,
        });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

// ── Audio buffer processing ───────────────────────────────────────────────

/// Extract float32 PCM from a `CMSampleBuffer`, convert to i16, accumulate,
/// and emit complete 960-sample frames (20 ms @ 48 kHz mono) via the
/// `wasapi_audio_frame` Tauri event.
///
/// ScreenCaptureKit delivers variable-size buffers (typically 512 or 1024
/// samples).  The accumulation pattern absorbs variable sizes and emits fixed
/// 960-sample frames — identical to the Windows WASAPI capture loop.
///
/// Channel handling:
/// - 1 channel in 1 buffer (mono): pass through.
/// - 2 channels, 1 buffer (interleaved stereo): downmix L+R → mono.
/// - 2 channels, 2 buffers (non-interleaved stereo): average left + right.
#[cfg(target_os = "macos")]
fn sck_process_audio_buffer(
    sample_buffer: &objc2_core_media::CMSampleBuffer,
    app: &tauri::AppHandle,
    accum: &std::sync::Mutex<Vec<i16>>,
) {
    use tauri::Emitter;

    const FRAME_SAMPLES: usize = 960; // 20 ms at 48 kHz mono

    use std::sync::atomic::{AtomicUsize, Ordering};

    // Diagnostic counters — eprintln always appears in the terminal.
    static EMIT_COUNT: AtomicUsize = AtomicUsize::new(0);
    static EARLY_RETURN_NOT_READY: AtomicUsize = AtomicUsize::new(0);
    static DIAG_GUARD_PRINTED: AtomicUsize = AtomicUsize::new(0);

    // ── Extract AudioBufferList via CoreMedia C API ────────────────

    // The CMSampleBuffer ObjC pointer is the same pointer as CMSampleBufferRef.
    let raw_sbuf =
        sample_buffer as *const objc2_core_media::CMSampleBuffer as *const std::ffi::c_void;

    // ScreenCaptureKit can deliver callbacks before the underlying sample data
    // is ready to be materialized into an AudioBufferList. Treat those as a
    // normal warm-up condition rather than a warning-worthy failure.
    if unsafe { !CMSampleBufferDataIsReady(raw_sbuf) } {
        let c = EARLY_RETURN_NOT_READY.fetch_add(1, Ordering::Relaxed);
        if c < 5 {
            eprintln!(
                "[wavis-diag] sck: GUARD buffer-not-ready (#{}) — skipping",
                c + 1
            );
        }
        return;
    }

    // Force the sample buffer data to be ready — SCK can deliver buffers
    // where DataIsReady returns true but the block buffer isn't materialized.
    let make_ready_status =
        unsafe { CMSampleBufferMakeDataReady(raw_sbuf as *const _ as *mut std::ffi::c_void) };

    // Diagnostic: number of samples in this buffer
    let num_samples = unsafe { CMSampleBufferGetNumSamples(raw_sbuf) };

    {
        static MAKE_READY_DIAG: AtomicUsize = AtomicUsize::new(0);
        let c = MAKE_READY_DIAG.fetch_add(1, Ordering::Relaxed);
        if c < 5 {
            eprintln!(
                "[wavis-diag] sck: MakeDataReady={} numSamples={}",
                make_ready_status, num_samples
            );
        }
    }

    // First call: query the exact buffer list size required.
    // kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment = 1 << 0
    const ASSURE_16_BYTE_ALIGNMENT: u32 = 1;
    let mut needed_size: usize = 0;
    let query_status = unsafe {
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            raw_sbuf,
            &mut needed_size,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            ASSURE_16_BYTE_ALIGNMENT,
            std::ptr::null_mut(),
        )
    };

    // Transient not-ready errors occur during startup and occasionally while
    // the ScreenCaptureKit stream is stabilizing; silently skip those buffers.
    if query_status != 0 {
        let c = DIAG_GUARD_PRINTED.fetch_add(1, Ordering::Relaxed);
        if c < 5 {
            eprintln!(
                "[wavis-diag] sck: GUARD query-status={} (transient={}) needed_size={}",
                query_status,
                sck_is_transient_audio_buffer_status(query_status),
                needed_size
            );
        }
        return;
    }

    // Validate that our stack allocation is sufficient.
    let stack_capacity = std::mem::size_of::<MacAudioBufferList>();
    if needed_size > stack_capacity {
        let c = DIAG_GUARD_PRINTED.fetch_add(1, Ordering::Relaxed);
        if c < 5 {
            eprintln!(
                "[wavis-diag] sck: GUARD stack-overflow needed={} capacity={}",
                needed_size, stack_capacity
            );
        }
        return;
    }

    let mut abl = MacAudioBufferList {
        mNumberBuffers: 0,
        mBuffers: [
            MacAudioBuffer {
                mNumberChannels: 0,
                mDataByteSize: 0,
                mData: std::ptr::null_mut(),
            },
            MacAudioBuffer {
                mNumberChannels: 0,
                mDataByteSize: 0,
                mData: std::ptr::null_mut(),
            },
        ],
    };
    let mut block_buffer_out: *mut std::ffi::c_void = std::ptr::null_mut();

    let status = unsafe {
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            raw_sbuf,
            std::ptr::null_mut(),
            &mut abl,
            needed_size,
            std::ptr::null(),
            std::ptr::null(),
            ASSURE_16_BYTE_ALIGNMENT,
            &mut block_buffer_out,
        )
    };

    if status != 0 {
        let c = DIAG_GUARD_PRINTED.fetch_add(1, Ordering::Relaxed);
        if c < 5 {
            eprintln!(
                "[wavis-diag] sck: GUARD extract-status={} (transient={}) needed={} stack_cap={} repr_size={}",
                status,
                sck_is_transient_audio_buffer_status(status),
                needed_size,
                stack_capacity,
                std::mem::size_of::<MacAudioBufferList>()
            );
        }
        if !block_buffer_out.is_null() {
            unsafe { CFRelease(block_buffer_out as *const _) };
        }
        return;
    }

    // ── Convert float32 PCM → i16 and accumulate ──────────────────

    let num_buffers = abl.mNumberBuffers as usize;
    if num_buffers == 0 {
        let c = DIAG_GUARD_PRINTED.fetch_add(1, Ordering::Relaxed);
        if c < 5 {
            eprintln!("[wavis-diag] sck: GUARD num_buffers=0");
        }
        if !block_buffer_out.is_null() {
            unsafe { CFRelease(block_buffer_out as *const _) };
        }
        return;
    }

    let buf0 = &abl.mBuffers[0];
    let n_channels_buf0 = buf0.mNumberChannels as usize;

    if buf0.mData.is_null() || buf0.mDataByteSize == 0 {
        let c = DIAG_GUARD_PRINTED.fetch_add(1, Ordering::Relaxed);
        if c < 5 {
            eprintln!(
                "[wavis-diag] sck: GUARD buf0 null={} byteSize={}",
                buf0.mData.is_null(),
                buf0.mDataByteSize
            );
        }
        if !block_buffer_out.is_null() {
            unsafe { CFRelease(block_buffer_out as *const _) };
        }
        return;
    }

    // ── Diagnostic: first successful buffer extraction ─────────────
    {
        static FIRST_BUFFER_DIAG: AtomicUsize = AtomicUsize::new(0);
        let c = FIRST_BUFFER_DIAG.fetch_add(1, Ordering::Relaxed);
        if c < 3 {
            let float_count_diag = buf0.mDataByteSize as usize / std::mem::size_of::<f32>();
            eprintln!(
                "[wavis-diag] sck: buffer-ok #{} — num_buffers={} ch={} byteSize={} floats={} needed_size={} stack_cap={}",
                c + 1, num_buffers, n_channels_buf0, buf0.mDataByteSize, float_count_diag, needed_size, stack_capacity
            );
        }
    }

    let float_count = buf0.mDataByteSize as usize / std::mem::size_of::<f32>();
    let float_slice = unsafe { std::slice::from_raw_parts(buf0.mData as *const f32, float_count) };

    let mut guard = match accum.lock() {
        Ok(g) => g,
        Err(_) => {
            if !block_buffer_out.is_null() {
                unsafe { CFRelease(block_buffer_out as *const _) };
            }
            return;
        }
    };

    if n_channels_buf0 == 1 {
        // Mono or non-interleaved left channel — use directly.
        // If a second buffer exists (right channel), we average below.
        if num_buffers >= 2 {
            let buf1 = &abl.mBuffers[1];
            if !buf1.mData.is_null() && buf1.mDataByteSize as usize >= float_count * 4 {
                // Non-interleaved stereo: average L + R.
                let right =
                    unsafe { std::slice::from_raw_parts(buf1.mData as *const f32, float_count) };
                for (&l, &r) in float_slice.iter().zip(right.iter()) {
                    let mono = (l + r) * 0.5;
                    guard.push((mono.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
            } else {
                // Right channel not available — use left only.
                for &s in float_slice {
                    guard.push((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
            }
        } else {
            // Single mono buffer.
            for &s in float_slice {
                guard.push((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            }
        }
    } else if n_channels_buf0 == 2 {
        // Interleaved stereo: L R L R … — downmix pairs to mono.
        let mut i = 0;
        while i + 1 < float_slice.len() {
            let mono = (float_slice[i] + float_slice[i + 1]) * 0.5;
            guard.push((mono.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            i += 2;
        }
    } else {
        // Unexpected channel count — use first channel only.
        let step = n_channels_buf0.max(1);
        let mut i = 0;
        while i < float_slice.len() {
            guard.push((float_slice[i].clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            i += step;
        }
    }

    // Release the CoreMedia block buffer retained by the C API.
    if !block_buffer_out.is_null() {
        unsafe { CFRelease(block_buffer_out as *const _) };
    }

    // ── Emit complete 960-sample frames via Tauri event ───────────

    while guard.len() >= FRAME_SAMPLES {
        let frame: Vec<i16> = guard.drain(..FRAME_SAMPLES).collect();
        let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
        let b64 = base64_encode(&bytes);

        if let Err(e) = app.emit("wasapi_audio_frame", &b64) {
            log::warn!("[audio_capture] sck: emit wasapi_audio_frame failed: {e}");
            let _ = app.emit(
                "wasapi_audio_stopped",
                serde_json::json!({ "reason": "Event emission failed" }),
            );
            return;
        }
        let ec = EMIT_COUNT.fetch_add(1, Ordering::Relaxed);
        if ec < 3 || ec % 500 == 0 {
            eprintln!(
                "[wavis-diag] sck: emitted frame #{} ({} bytes b64)",
                ec + 1,
                b64.len()
            );
        }
    }
}

// ── collect_child_pids ────────────────────────────────────────────────────

/// Enumerate all direct child processes of `parent_pid` using `pgrep`.
///
/// Shells out to `pgrep -P <parent_pid>` which returns one PID per line.
/// On any failure (pgrep not found, no children, etc.), logs a warning and
/// returns an empty vec so the caller can proceed without child exclusion.
///
/// NOTE: This function is retained for potential future use but the SCK path
/// now uses `enumerate_wavis_pids()` instead, which catches WKWebView helpers
/// reparented to launchd (PPID=1) that `pgrep -P` misses.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn collect_child_pids(parent_pid: u32) -> Vec<i32> {
    use std::process::Command;

    let output = match Command::new("pgrep")
        .arg("-P")
        .arg(parent_pid.to_string())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log::warn!("[audio_capture] sck: pgrep failed: {e}; child process exclusion skipped");
            return Vec::new();
        }
    };

    // pgrep exits 1 when no processes match — not an error for us.
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .collect()
}

// ── enumerate_wavis_pids ──────────────────────────────────────────────────

/// Enumerate all Wavis-related process IDs for Core Audio tap exclusion.
///
/// Uses libproc to find executables that belong to the current Wavis app
/// bundle (or closely related helper binaries in the same directory) so the
/// Core Audio process tap can exclude them together.
#[cfg(target_os = "macos")]
fn enumerate_wavis_pids() -> Vec<i32> {
    let main_pid = std::process::id() as i32;
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            log::warn!(
                "[audio_capture] tap: current_exe failed: {err}; falling back to main PID only"
            );
            eprintln!("[wavis-diag] tap: excluding main PID only (current_exe failed): {main_pid}");
            return vec![main_pid];
        }
    };
    let current_dir = current_exe
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("/"));
    let bundle_root = find_app_bundle_root(&current_exe);
    let app_name_key = current_exe
        .file_stem()
        .and_then(|name| name.to_str())
        .map(normalize_process_name)
        .unwrap_or_default();

    eprintln!("[wavis-diag] tap: current_exe={}", current_exe.display());
    if let Some(root) = &bundle_root {
        eprintln!("[wavis-diag] tap: bundle_root={}", root.display());
    } else {
        eprintln!("[wavis-diag] tap: bundle_root=<none>");
    }

    let mut matched_pids = std::collections::BTreeSet::new();
    matched_pids.insert(main_pid);

    for pid in list_all_pids() {
        let Some(pid_path) = pid_executable_path(pid) else {
            continue;
        };
        let Some(reason) = is_wavis_related_executable(
            &pid_path,
            &current_exe,
            bundle_root.as_deref(),
            &current_dir,
            &app_name_key,
        ) else {
            continue;
        };

        if matched_pids.insert(pid) {
            eprintln!(
                "[wavis-diag] tap: matched pid={} reason={} path={}",
                pid,
                reason,
                pid_path.display()
            );
        }
    }

    let pids: Vec<i32> = matched_pids.into_iter().collect();
    eprintln!(
        "[wavis-diag] tap: excluding {} pid(s): {:?}",
        pids.len(),
        pids
    );
    pids
}

// ── tap_ioproc ────────────────────────────────────────────────────────────

/// CoreAudio IOProc callback for the system-audio process tap.
///
/// Runs on CoreAudio's real-time I/O thread.  Reads float32 samples from the
/// input `AudioBufferList`, converts them to i16 PCM, accumulates 960-sample
/// frames and emits `wasapi_audio_frame` Tauri events (same as the SCK path).
///
/// # Safety
/// `in_client_data` must be a raw pointer to an `Arc<IoProcCtx>` that remains
/// valid for the duration of the IOProc (guaranteed by `TapCaptureHandle`
/// keeping the Arc alive until `AudioDeviceDestroyIOProcID` returns).
#[cfg(target_os = "macos")]
unsafe extern "C" fn tap_ioproc(
    _device: u32,
    _in_now: *const std::ffi::c_void,
    in_input_data: *const MacAudioBufferList,
    _in_input_time: *const std::ffi::c_void,
    _out_output_data: *mut MacAudioBufferList,
    _in_output_time: *const std::ffi::c_void,
    in_client_data: *mut std::ffi::c_void,
) -> i32 {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;
    const FRAME_SAMPLES: usize = 960;

    // SAFETY: caller guarantees in_client_data is Arc<IoProcCtx> raw ptr.
    let ctx = &*(in_client_data as *const IoProcCtx);

    if ctx.stop_flag.load(Ordering::Relaxed) {
        return K_AUDIO_HARDWARE_NO_ERROR;
    }

    // ── One-shot "first frame" signal ─────────────────────────────
    // try_lock avoids blocking on the real-time thread; if the lock is
    // momentarily contended, the signal fires on the next callback.
    if let Ok(mut tx_guard) = ctx.first_frame_tx.try_lock() {
        if let Some(tx) = tx_guard.take() {
            let _ = tx.send(());
        }
    }

    // ── Read audio samples ─────────────────────────────────────────
    let abl = &*in_input_data;
    if abl.mNumberBuffers == 0 {
        return K_AUDIO_HARDWARE_NO_ERROR;
    }
    let buf0 = &abl.mBuffers[0];
    if buf0.mData.is_null() || buf0.mDataByteSize == 0 {
        return K_AUDIO_HARDWARE_NO_ERROR;
    }

    let float_count = buf0.mDataByteSize as usize / std::mem::size_of::<f32>();
    let floats = std::slice::from_raw_parts(buf0.mData as *const f32, float_count);

    // ── Accumulate i16 samples ─────────────────────────────────────
    let mut guard = match ctx.accum.try_lock() {
        Ok(g) => g,
        Err(_) => return K_AUDIO_HARDWARE_NO_ERROR, // avoid blocking on RT thread
    };

    let n_channels = buf0.mNumberChannels;
    if n_channels == 1 {
        if abl.mNumberBuffers >= 2 {
            let buf1 = &abl.mBuffers[1];
            if !buf1.mData.is_null() && buf1.mDataByteSize as usize >= float_count * 4 {
                // Non-interleaved stereo: average L + R.
                let right = std::slice::from_raw_parts(buf1.mData as *const f32, float_count);
                for (&l, &r) in floats.iter().zip(right.iter()) {
                    let mono = (l + r) * 0.5;
                    guard.push((mono.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
            } else {
                for &s in floats {
                    guard.push((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                }
            }
        } else {
            // Single mono buffer.
            for &s in floats {
                guard.push((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            }
        }
    } else if n_channels == 2 {
        // Interleaved stereo: L R L R … → downmix to mono.
        let mut i = 0;
        while i + 1 < floats.len() {
            let mono = (floats[i] + floats[i + 1]) * 0.5;
            guard.push((mono.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            i += 2;
        }
    } else {
        // Unexpected channel count — use first channel only.
        let step = n_channels.max(1) as usize;
        let mut i = 0;
        while i < floats.len() {
            guard.push((floats[i].clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            i += step;
        }
    }

    // ── Emit 960-sample frames ─────────────────────────────────────
    // Diagnostic frame counter — logs first frame and every ~500 frames (~10 s
    // at 50 Hz) so the tap's liveness is visible without flooding the log.
    use std::sync::atomic::{AtomicUsize, Ordering as AO};
    static TAP_EMIT_COUNT: AtomicUsize = AtomicUsize::new(0);

    while guard.len() >= FRAME_SAMPLES {
        let frame: Vec<i16> = guard.drain(..FRAME_SAMPLES).collect();
        let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
        let b64 = base64_encode(&bytes);

        let n = TAP_EMIT_COUNT.fetch_add(1, AO::Relaxed) + 1;
        if n == 1 || n % 500 == 0 {
            log::debug!(
                "[audio_capture] tap: emitted frame #{n} ({} bytes)",
                bytes.len()
            );
        }

        if let Err(e) = ctx.app.emit("wasapi_audio_frame", &b64) {
            log::warn!("[audio_capture] tap: emit wasapi_audio_frame failed: {e}");
            let _ = ctx.app.emit(
                "wasapi_audio_stopped",
                serde_json::json!({ "reason": "Event emission failed" }),
            );
            ctx.stop_flag.store(true, Ordering::Relaxed);
            return K_AUDIO_HARDWARE_NO_ERROR;
        }
    }

    K_AUDIO_HARDWARE_NO_ERROR
}

// ── audio_queue_input_callback ────────────────────────────────────────────

/// AudioQueue input callback for the virtual-device capture path.
///
/// Called on AudioQueue's internal thread with a filled buffer from BlackHole's
/// input stream (which mirrors the aggregate's output — the system audio).
/// Converts float32 PCM to i16, accumulates 960-sample frames, and emits
/// `wasapi_audio_frame` Tauri events identical to the tap and SCK paths.
///
/// The buffer is re-enqueued at the end so the queue keeps running.
///
/// # Safety
/// `in_user_data` must be a raw pointer to an `Arc<IoProcCtx>` that remains
/// valid for the duration of the queue (guaranteed by `VirtualDeviceCaptureHandle`
/// keeping the Arc alive until `AudioQueueDispose` returns).
#[cfg(target_os = "macos")]
unsafe extern "C" fn audio_queue_input_callback(
    in_user_data: *mut std::ffi::c_void,
    in_aq: *mut std::ffi::c_void,
    in_buffer: *mut AudioQueueBuffer,
    _in_start_time: *const std::ffi::c_void,
    _in_num_packet_descs: u32,
    _in_packet_descs: *const std::ffi::c_void,
) {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;
    const FRAME_SAMPLES: usize = 960;

    let ctx = &*(in_user_data as *const IoProcCtx);

    if ctx.stop_flag.load(Ordering::Relaxed) {
        // Stop flag set — don't re-enqueue; let the queue drain naturally.
        return;
    }

    // One-shot "first frame" signal.
    if let Ok(mut tx_guard) = ctx.first_frame_tx.try_lock() {
        if let Some(tx) = tx_guard.take() {
            let _ = tx.send(());
        }
    }

    let buf = &*in_buffer;
    if buf.m_audio_data.is_null() || buf.m_audio_data_byte_size == 0 {
        AudioQueueEnqueueBuffer(in_aq, in_buffer, 0, std::ptr::null());
        return;
    }

    let float_count = buf.m_audio_data_byte_size as usize / std::mem::size_of::<f32>();
    let floats = std::slice::from_raw_parts(buf.m_audio_data as *const f32, float_count);

    let mut guard = match ctx.accum.try_lock() {
        Ok(g) => g,
        Err(_) => {
            AudioQueueEnqueueBuffer(in_aq, in_buffer, 0, std::ptr::null());
            return;
        }
    };

    let n_channels = ctx.native_channels;
    if n_channels == 1 {
        for &s in floats {
            guard.push((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        }
    } else if n_channels == 2 {
        // Interleaved stereo: L R L R … → downmix to mono.
        let mut i = 0;
        while i + 1 < floats.len() {
            let mono = (floats[i] + floats[i + 1]) * 0.5;
            guard.push((mono.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            i += 2;
        }
    } else {
        // Unexpected channel count — use first channel only.
        let step = n_channels.max(1) as usize;
        let mut i = 0;
        while i < floats.len() {
            guard.push((floats[i].clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            i += step;
        }
    }

    use std::sync::atomic::{AtomicUsize, Ordering as AO};
    static AQ_EMIT_COUNT: AtomicUsize = AtomicUsize::new(0);

    while guard.len() >= FRAME_SAMPLES {
        let frame: Vec<i16> = guard.drain(..FRAME_SAMPLES).collect();
        let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
        let b64 = base64_encode(&bytes);

        let n = AQ_EMIT_COUNT.fetch_add(1, AO::Relaxed) + 1;
        if n == 1 || n % 500 == 0 {
            log::debug!(
                "[audio_capture] virtual-device: emitted audio queue frame #{n} ({} bytes)",
                bytes.len()
            );
        }

        if let Err(e) = ctx.app.emit("wasapi_audio_frame", &b64) {
            log::warn!("[audio_capture] virtual-device: emit wasapi_audio_frame failed: {e}");
            ctx.stop_flag.store(true, Ordering::Relaxed);
            // Don't re-enqueue — let the queue drain.
            return;
        }
    }

    drop(guard);

    // Re-enqueue for next callback.
    AudioQueueEnqueueBuffer(in_aq, in_buffer, 0, std::ptr::null());
}

// ── audio_share_start_tap ─────────────────────────────────────────────────

/// Start system-audio capture via a Core Audio process tap (macOS 14.2+).
///
/// Builds a `CATapDescription` that captures ALL system audio EXCEPT the
/// Wavis-related processes (main PID + WebKit helpers), registers an IOProc
/// callback, and starts capture.  Returns `loopback_exclusion_available: true`.
#[cfg(target_os = "macos")]
fn audio_share_start_tap(
    source_id: String,
    guard: &mut Option<MacAudioHandle>,
    app: tauri::AppHandle,
) -> Result<AudioShareStartResult, String> {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::ClassType;
    use objc2_foundation::{NSArray, NSMutableArray};
    use std::ffi::c_void;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // ── 1. Enumerate Wavis-related PIDs ───────────────────────────
    let wavis_pids = enumerate_wavis_pids();

    // ── 2. Build NSArray<NSNumber> of PIDs for CATapDescription ───
    // Use dynamic ObjC runtime to create NSNumber objects — avoids needing
    // the NSNumber feature flag from objc2-foundation.
    let ns_number_cls: &'static AnyClass =
        AnyClass::get(c"NSNumber").ok_or_else(|| "NSNumber ObjC class not found".to_string())?;

    let pid_mut_arr: Retained<NSMutableArray<AnyObject>> =
        unsafe { objc2::msg_send![NSMutableArray::<AnyObject>::class(), array] };

    for &pid in &wavis_pids {
        // [NSNumber numberWithInt:] returns an autoreleased NSNumber.
        // addObject: immediately retains it, so the autorelease is safe.
        let num: *mut AnyObject =
            unsafe { objc2::msg_send![ns_number_cls, numberWithInt: pid as i32] };
        unsafe {
            let _: () = objc2::msg_send![&*pid_mut_arr, addObject: num];
        }
    }

    let pid_array: Retained<NSArray<AnyObject>> =
        unsafe { Retained::cast_unchecked::<NSArray<AnyObject>>(pid_mut_arr) };

    // ── 3. Create CATapDescription ─────────────────────────────────
    let tap_desc_cls: &'static AnyClass = AnyClass::get(c"CATapDescription").ok_or_else(|| {
        "CATapDescription ObjC class not found (requires macOS 14.2+)".to_string()
    })?;

    let alloc_ptr: *mut AnyObject = unsafe { objc2::msg_send![tap_desc_cls, alloc] };
    let desc_ptr: *mut AnyObject =
        unsafe { objc2::msg_send![alloc_ptr, initStereoGlobalTapButExcludeProcesses: &*pid_array] };
    if desc_ptr.is_null() {
        return Err("CATapDescription init returned nil".to_string());
    }
    // exclusive = true → capture ALL processes EXCEPT those listed.
    unsafe {
        let _: () = objc2::msg_send![desc_ptr, setExclusive: true];
    }

    // ── 4. Create the process tap ─────────────────────────────────
    let mut tap_id: u32 = 0;
    let tap_status = unsafe { AudioHardwareCreateProcessTap(desc_ptr as *mut c_void, &mut tap_id) };
    // Release the description — CoreAudio has retained it if it succeeded.
    // Use objc_release directly since desc_ptr is a raw *mut AnyObject.
    extern "C" {
        fn objc_release(obj: *mut std::ffi::c_void);
    }
    unsafe { objc_release(desc_ptr as *mut c_void) };

    if tap_status != 0 {
        return Err(format!(
            "AudioHardwareCreateProcessTap failed: OSStatus {tap_status}"
        ));
    }
    log::info!("[audio_capture] tap: created tap AudioObjectID={tap_id}");

    // ── 5. Query native audio format ──────────────────────────────
    let addr = TapPropertyAddress {
        m_selector: K_AUDIO_DEVICE_PROPERTY_STREAM_FORMAT,
        m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_INPUT,
        m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut asbd = TapStreamBasicDescription {
        m_sample_rate: 0.0,
        m_format_id: 0,
        m_format_flags: 0,
        m_bytes_per_packet: 0,
        m_frames_per_packet: 0,
        m_bytes_per_frame: 0,
        m_channels_per_frame: 0,
        m_bits_per_channel: 0,
        m_reserved: 0,
    };
    let mut asbd_size = std::mem::size_of::<TapStreamBasicDescription>() as u32;
    let fmt_status = unsafe {
        AudioObjectGetPropertyData(
            tap_id,
            &addr,
            0,
            std::ptr::null(),
            &mut asbd_size,
            &mut asbd as *mut _ as *mut c_void,
        )
    };
    if fmt_status != 0 {
        unsafe { AudioHardwareDestroyProcessTap(tap_id) };
        return Err(format!(
            "AudioObjectGetPropertyData (stream format) failed: OSStatus {fmt_status}"
        ));
    }
    let native_rate = asbd.m_sample_rate;
    let native_channels = asbd.m_channels_per_frame;
    log::info!(
        "[audio_capture] tap: native format — {native_rate}Hz, \
         {native_channels}ch, formatID=0x{:08X}",
        asbd.m_format_id
    );
    if native_rate != 48000.0 {
        log::warn!(
            "[audio_capture] tap: native sample rate {native_rate}Hz ≠ 48kHz; \
             audio will be pitch-shifted — resampling not yet implemented"
        );
    }

    // ── 6. Create IOProc context ──────────────────────────────────
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let ctx = Arc::new(IoProcCtx {
        app: app.clone(),
        accum: Mutex::new(Vec::with_capacity(960 * 4)),
        stop_flag: stop_flag.clone(),
        first_frame_tx: Mutex::new(Some(frame_tx)),
        native_rate,
        native_channels,
    });

    // ── 7. Register IOProc ────────────────────────────────────────
    let ctx_ptr = Arc::as_ptr(&ctx) as *mut c_void;
    let mut proc_id: *mut c_void = std::ptr::null_mut();
    let ioproc_status =
        unsafe { AudioDeviceCreateIOProcID(tap_id, tap_ioproc, ctx_ptr, &mut proc_id) };
    if ioproc_status != 0 {
        unsafe { AudioHardwareDestroyProcessTap(tap_id) };
        return Err(format!(
            "AudioDeviceCreateIOProcID failed: OSStatus {ioproc_status}"
        ));
    }

    // ── 8. Start capture ──────────────────────────────────────────
    let start_status = unsafe { AudioDeviceStart(tap_id, proc_id) };
    if start_status != 0 {
        unsafe {
            AudioDeviceDestroyIOProcID(tap_id, proc_id);
            AudioHardwareDestroyProcessTap(tap_id);
        }
        return Err(format!("AudioDeviceStart failed: OSStatus {start_status}"));
    }

    // ── 9. Wait for first audio frame (diagnostic, non-fatal) ─────
    // Confirms the tap is delivering audio.  If silent (no system audio is
    // playing), we warn but do not fail — the tap is still valid.
    match frame_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => {
            log::info!("[audio_capture] tap: first audio frame received — tap is live");
        }
        Err(_) => {
            log::warn!(
                "[audio_capture] tap: no audio received within 5 s \
                 (tap may be valid but silent if no system audio is playing)"
            );
        }
    }

    // ── 10. Store handle ──────────────────────────────────────────
    *guard = Some(MacAudioHandle::Tap(TapCaptureHandle {
        tap_id,
        proc_id,
        stop_flag,
        source_id,
        _ctx: ctx,
    }));

    log::info!(
        "[audio_capture] tap: capturing system audio via Core Audio process tap (macOS 14.2+)"
    );

    Ok(AudioShareStartResult {
        loopback_exclusion_available: true,
        real_output_device_id: None,
        real_output_device_name: None,
        requires_mute_for_echo_prevention: false,
    })
}

// ── audio_share_start_macos ───────────────────────────────────────────────

// Start flow documentation for `audio_share_start_macos` lives with the
// function definition below.
/// 1. Runtime version guard (SCK requires 12.3+).
/// 2. On macOS 14.2+, try the Core Audio process-tap path first.
/// 3. If the tap path is unavailable, fall back to ScreenCaptureKit.
/// 4. Fetch `SCShareableContent` (async → sync bridge, 3 s timeout).
/// 5. Build content filter for the primary display, excluding the Wavis process.
/// 6. Create `SCStreamConfiguration` with audio-only settings (48 kHz mono).
/// 7. Create `SCStream`, attach `AudioOutputHandler` delegate.
/// 8. Start capture (async → sync bridge, 5 s timeout).
/// 9. Store handle in `AudioCaptureState`.
// Best-effort rollback for partial virtual-device setup failures.
// All rollback paths in `audio_share_start_virtual_device` dispose the
// AudioQueue themselves before calling this helper (so queue is always null
// here); this function only handles routing state cleanup.
#[cfg(target_os = "macos")]
fn rollback_virtual_device_start(
    audio_queue: Option<*mut std::ffi::c_void>,
    original_default_output: Option<u32>,
    aggregate_device_id: Option<u32>,
) {
    share_audio_info!(
        "[audio_capture] virtual-device: rollback start original_default_output={:?} \
         aggregate_device_id={:?}",
        original_default_output,
        aggregate_device_id
    );

    if let Some(queue) = audio_queue {
        if !queue.is_null() {
            unsafe {
                let stop_status = AudioQueueStop(queue, 1);
                if stop_status != 0 {
                    log::warn!(
                        "[audio_capture] virtual-device: rollback AudioQueueStop failed: \
                         OSStatus {stop_status}"
                    );
                }
                let dispose_status = AudioQueueDispose(queue, 1);
                if dispose_status != 0 {
                    log::warn!(
                        "[audio_capture] virtual-device: rollback AudioQueueDispose failed: \
                         OSStatus {dispose_status}"
                    );
                }
            }
        }
    }

    if let Some(original_default_output) = original_default_output {
        if let Err(err) = restore_system_default_output(original_default_output) {
            log::warn!(
                "[audio_capture] virtual-device: rollback restore default output failed: {err}"
            );
        }
    }

    if let Some(aggregate_device_id) = aggregate_device_id {
        if let Err(err) = destroy_multi_output_device(aggregate_device_id) {
            log::warn!(
                "[audio_capture] virtual-device: rollback destroy aggregate device failed: {err}"
            );
        }
    }

    share_audio_info!("[audio_capture] virtual-device: rollback complete");
}

#[cfg(target_os = "macos")]
fn teardown_virtual_device_routing(
    routing_state: &mut VirtualDeviceRoutingState,
    trigger: VirtualDeviceTeardownTrigger,
    reason: &str,
) {
    use std::sync::atomic::Ordering;
    let actions = plan_virtual_device_teardown(
        trigger,
        VirtualDeviceTeardownSnapshot {
            original_default_output: routing_state.original_default_output,
            aggregate_device_id: routing_state.aggregate_device_id,
            audio_queue_registered: !routing_state.audio_queue.is_null(),
        },
    );

    share_audio_info!(
        "[audio_capture] virtual-device: teardown start reason='{}' original_default_output={} \
         aggregate_device_id={}",
        reason,
        routing_state.original_default_output,
        routing_state.aggregate_device_id,
    );

    routing_state.stop_flag.store(true, Ordering::Relaxed);

    for action in actions {
        match action {
            VirtualDeviceTeardownAction::RestoreDefaultOutput(original_default_output) => {
                if let Err(err) = restore_system_default_output(original_default_output) {
                    log::warn!(
                        "[audio_capture] virtual-device: {reason} restore default output failed: \
                         {err}"
                    );
                }
                routing_state.original_default_output = 0;
            }
            VirtualDeviceTeardownAction::StopAudioQueue => unsafe {
                let stop_status = AudioQueueStop(routing_state.audio_queue, 1);
                if stop_status != 0 {
                    log::warn!(
                        "[audio_capture] virtual-device: {reason} AudioQueueStop failed: \
                         OSStatus {stop_status}"
                    );
                }
            },
            VirtualDeviceTeardownAction::DestroyAudioQueue => unsafe {
                let dispose_status = AudioQueueDispose(routing_state.audio_queue, 1);
                if dispose_status != 0 {
                    log::warn!(
                        "[audio_capture] virtual-device: {reason} AudioQueueDispose failed: \
                         OSStatus {dispose_status}"
                    );
                }
                routing_state.audio_queue = std::ptr::null_mut();
            },
            VirtualDeviceTeardownAction::DestroyAggregateDevice(aggregate_device_id) => {
                if let Err(err) = destroy_multi_output_device(aggregate_device_id) {
                    log::warn!(
                        "[audio_capture] virtual-device: {reason} destroy aggregate device \
                         failed: {err}"
                    );
                }
                routing_state.aggregate_device_id = 0;
            }
        }
    }

    share_audio_info!(
        "[audio_capture] virtual-device: teardown complete reason='{}'",
        reason
    );
}

#[cfg(target_os = "macos")]
impl Drop for VirtualDeviceRoutingState {
    fn drop(&mut self) {
        teardown_virtual_device_routing(self, VirtualDeviceTeardownTrigger::Drop, "drop cleanup");
    }
}

/// Start system-audio capture via a virtual loopback device (for example
/// BlackHole) routed through a temporary multi-output device.
///
/// Returns `Ok(None)` when no supported virtual device is installed so the
/// caller can continue down the fallback chain.
#[cfg(target_os = "macos")]
fn audio_share_start_virtual_device(
    source_id: String,
    guard: &mut Option<MacAudioHandle>,
    app: tauri::AppHandle,
) -> Result<Option<AudioShareStartResult>, String> {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    share_audio_info!("[audio_capture] virtual-device: startup attempt begin");

    if let Err(err) = cleanup_stale_multi_output_devices() {
        log::warn!(
            "[audio_capture] virtual-device: stale bridge cleanup failed before startup: {err}"
        );
    }

    let Some(virtual_device) = detect_virtual_audio_device() else {
        log::warn!(
            "[audio_capture] virtual-device: no supported loopback device found \
             (BlackHole 2ch/16ch, Loopback, or WAVIS_LOOPBACK_DEVICE); \
             skipping virtual-device fallback"
        );
        return Ok(None);
    };

    log::info!(
        "[audio_capture] virtual-device: using loopback device {} '{}' uid='{}'",
        virtual_device.device_id,
        virtual_device.name,
        virtual_device.uid
    );

    let real_output_uid = get_real_output_device_uid()?;

    // ── Bypass path: default output already contains BlackHole ────────────────
    // When the system default output is already a stacked aggregate that
    // includes BlackHole as a sub-device (e.g. a multi-output device created in
    // Audio MIDI Setup), creating a second Wavis Bridge on top of it would nest
    // an aggregate inside an aggregate.  CoreAudio does not support routing
    // through nested stacked aggregates — the inner device's hardware outputs
    // are not reachable — so speakers go silent.
    //
    // Bypass: skip bridge creation, leave the default output untouched, and
    // attach the AudioQueue directly to BlackHole's input.  The existing device
    // already routes system audio to BlackHole (loopback) and to speakers.
    let bypass_hardware_uid: Option<String> = {
        // Resolve the current default output device ID to pass to the sub-device
        // inspector (we only have the UID from get_real_output_device_uid).
        let addr = TapPropertyAddress {
            m_selector: 0x644F_7574, // kAudioHardwarePropertyDefaultOutputDevice
            m_scope: 0x676C_6F62,    // kAudioObjectPropertyScopeGlobal
            m_element: 0,
        };
        let mut default_id = 0u32;
        let mut sz = std::mem::size_of::<u32>() as u32;
        unsafe {
            AudioObjectGetPropertyData(
                1, // kAudioObjectSystemObject
                &addr,
                0,
                std::ptr::null(),
                &mut sz,
                &mut default_id as *mut _ as *mut std::ffi::c_void,
            )
        };
        find_hardware_speaker_uid_in_aggregate(default_id, &virtual_device.uid)
    };

    // `original_default_output_opt` / `aggregate_device_id_opt` are `None` on
    // the bypass path (nothing to restore / destroy on teardown).
    let (original_default_output_opt, aggregate_device_id_opt, result_uid) =
        if let Some(hw_uid) = bypass_hardware_uid {
            log::info!(
                "[audio_capture] virtual-device: bypass — default output already routes to '{}'; \
                 skipping Wavis Bridge; hardware speaker uid='{hw_uid}'",
                virtual_device.name,
            );
            (None::<u32>, None::<u32>, hw_uid)
        } else {
            log::info!("[audio_capture] virtual-device: real output uid='{real_output_uid}'");
            let agg_id = create_multi_output_device(&real_output_uid, &virtual_device.uid)?;
            let orig = match swap_system_default_output(agg_id) {
                Ok(device_id) => device_id,
                Err(err) => {
                    rollback_virtual_device_start(None, None, Some(agg_id));
                    return Err(err);
                }
            };
            (Some(orig), Some(agg_id), real_output_uid)
        };
    let addr = TapPropertyAddress {
        m_selector: K_AUDIO_DEVICE_PROPERTY_STREAM_FORMAT,
        m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_INPUT,
        m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };
    let mut asbd = TapStreamBasicDescription {
        m_sample_rate: 0.0,
        m_format_id: 0,
        m_format_flags: 0,
        m_bytes_per_packet: 0,
        m_frames_per_packet: 0,
        m_bytes_per_frame: 0,
        m_channels_per_frame: 0,
        m_bits_per_channel: 0,
        m_reserved: 0,
    };
    let mut asbd_size = std::mem::size_of::<TapStreamBasicDescription>() as u32;
    let fmt_status = unsafe {
        AudioObjectGetPropertyData(
            virtual_device.device_id,
            &addr,
            0,
            std::ptr::null(),
            &mut asbd_size,
            &mut asbd as *mut _ as *mut std::ffi::c_void,
        )
    };
    if fmt_status != 0 {
        rollback_virtual_device_start(None, original_default_output_opt, aggregate_device_id_opt);
        return Err(format!(
            "virtual-device stream format lookup failed for device {}: OSStatus {fmt_status}",
            virtual_device.device_id
        ));
    }

    let native_rate = asbd.m_sample_rate;
    let native_channels = asbd.m_channels_per_frame;
    share_audio_info!(
        "[audio_capture] virtual-device: native format for '{}' is {native_rate}Hz, \
         {native_channels}ch, formatID=0x{:08X}",
        virtual_device.name,
        asbd.m_format_id
    );
    if native_rate != 48000.0 {
        log::warn!(
            "[audio_capture] virtual-device: native sample rate {native_rate}Hz != 48kHz; \
             audio will be pitch-shifted - resampling not yet implemented"
        );
    }

    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let ctx = Arc::new(IoProcCtx {
        app: app.clone(),
        accum: Mutex::new(Vec::with_capacity(960 * 4)),
        stop_flag: stop_flag.clone(),
        first_frame_tx: Mutex::new(Some(frame_tx)),
        native_rate,
        native_channels,
    });

    // ── AudioQueue input on BlackHole — no IOProc on the aggregate ────────────
    //
    // Registering an IOProc on the aggregate puts it in "pull mode": the HAL
    // stops passive output routing and expects the IOProc to fill outOutputData
    // for the speakers sub-device. Since we only want to read from BlackHole's
    // loopback input, we use AudioQueueNewInput instead. The queue reads from
    // BlackHole's input stream without touching the aggregate's output path, so
    // the stacked multi-output device continues to route audio passively to both
    // speakers and BlackHole.

    let ctx_ptr = Arc::as_ptr(&ctx) as *mut std::ffi::c_void;

    // Request float32 interleaved PCM at BlackHole's native rate. CoreAudio
    // will handle format conversion if BlackHole delivers a different layout.
    let aq_format = TapStreamBasicDescription {
        m_sample_rate: native_rate,
        m_format_id: K_AUDIO_FORMAT_LINEAR_PCM,
        m_format_flags: K_AUDIO_FORMAT_FLAGS_FLOAT_PACKED,
        m_bytes_per_packet: native_channels * 4,
        m_frames_per_packet: 1,
        m_bytes_per_frame: native_channels * 4,
        m_channels_per_frame: native_channels,
        m_bits_per_channel: 32,
        m_reserved: 0,
    };

    let mut queue: *mut std::ffi::c_void = std::ptr::null_mut();
    let aq_new_status = unsafe {
        AudioQueueNewInput(
            &aq_format,
            audio_queue_input_callback,
            ctx_ptr,
            std::ptr::null(), // NULL → AudioQueue manages its own run-loop thread
            std::ptr::null(),
            0,
            &mut queue,
        )
    };
    if aq_new_status != 0 {
        rollback_virtual_device_start(None, original_default_output_opt, aggregate_device_id_opt);
        return Err(format!(
            "virtual-device AudioQueueNewInput failed for '{}': OSStatus {aq_new_status}",
            virtual_device.name
        ));
    }

    // Point the queue at BlackHole's input device via its UID (CFStringRef).
    let uid_cstr = match std::ffi::CString::new(virtual_device.uid.as_str()) {
        Ok(s) => s,
        Err(e) => {
            unsafe { AudioQueueDispose(queue, 1) };
            rollback_virtual_device_start(
                None,
                original_default_output_opt,
                aggregate_device_id_opt,
            );
            return Err(format!("virtual-device: invalid BlackHole UID string: {e}"));
        }
    };
    let cf_uid = unsafe {
        CFStringCreateWithCString(
            std::ptr::null(),
            uid_cstr.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if cf_uid.is_null() {
        unsafe { AudioQueueDispose(queue, 1) };
        rollback_virtual_device_start(None, original_default_output_opt, aggregate_device_id_opt);
        return Err(
            "virtual-device: CFStringCreateWithCString returned NULL for BlackHole UID".to_string(),
        );
    }
    let uid_set_status = unsafe {
        AudioQueueSetProperty(
            queue,
            K_AUDIO_QUEUE_PROPERTY_CURRENT_DEVICE,
            &cf_uid as *const *const std::ffi::c_void as *const std::ffi::c_void,
            std::mem::size_of::<*const std::ffi::c_void>() as u32,
        )
    };
    unsafe { CFRelease(cf_uid) };
    if uid_set_status != 0 {
        unsafe { AudioQueueDispose(queue, 1) };
        rollback_virtual_device_start(None, original_default_output_opt, aggregate_device_id_opt);
        return Err(format!(
            "virtual-device AudioQueueSetProperty CurrentDevice failed for '{}': \
             OSStatus {uid_set_status}",
            virtual_device.name
        ));
    }

    // Pre-allocate and enqueue buffers before starting.
    // 4096 frames × channels × 4 bytes/sample ≈ 85 ms at 48 kHz per buffer.
    const NUM_AQ_BUFFERS: usize = 3;
    let buffer_bytes = 4096 * native_channels * 4;
    for _ in 0..NUM_AQ_BUFFERS {
        let mut buf_ref: *mut AudioQueueBuffer = std::ptr::null_mut();
        let alloc_status = unsafe { AudioQueueAllocateBuffer(queue, buffer_bytes, &mut buf_ref) };
        if alloc_status != 0 {
            unsafe { AudioQueueDispose(queue, 1) };
            rollback_virtual_device_start(
                None,
                original_default_output_opt,
                aggregate_device_id_opt,
            );
            return Err(format!(
                "virtual-device AudioQueueAllocateBuffer failed for '{}': OSStatus {alloc_status}",
                virtual_device.name
            ));
        }
        let enq_status = unsafe { AudioQueueEnqueueBuffer(queue, buf_ref, 0, std::ptr::null()) };
        if enq_status != 0 {
            unsafe { AudioQueueDispose(queue, 1) };
            rollback_virtual_device_start(
                None,
                original_default_output_opt,
                aggregate_device_id_opt,
            );
            return Err(format!(
                "virtual-device AudioQueueEnqueueBuffer failed for '{}': OSStatus {enq_status}",
                virtual_device.name
            ));
        }
    }

    let aq_start_status = unsafe { AudioQueueStart(queue, std::ptr::null()) };
    if aq_start_status != 0 {
        unsafe { AudioQueueDispose(queue, 1) };
        rollback_virtual_device_start(None, original_default_output_opt, aggregate_device_id_opt);
        return Err(format!(
            "virtual-device AudioQueueStart failed for '{}': OSStatus {aq_start_status}",
            virtual_device.name
        ));
    }

    log::info!(
        "[audio_capture] virtual-device: AudioQueue started on '{}'; \
         aggregate={} waiting for first frame (up to 5 s)",
        virtual_device.name,
        aggregate_device_id_opt.unwrap_or(0),
    );

    match frame_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => {
            log::info!(
                "[audio_capture] virtual-device: first audio frame received from '{}' — capture \
                 is live; aggregate={} result_uid='{result_uid}'",
                virtual_device.name,
                aggregate_device_id_opt.unwrap_or(0),
            );
        }
        Err(_) => {
            log::warn!(
                "[audio_capture] virtual-device: no audio received from '{}' within 5 s — \
                 aggregate={} may not be routing (is BlackHole loopback working?)",
                virtual_device.name,
                aggregate_device_id_opt.unwrap_or(0),
            );
        }
    }

    *guard = Some(MacAudioHandle::VirtualDevice(VirtualDeviceCaptureHandle {
        routing_state: VirtualDeviceRoutingState {
            original_default_output: original_default_output_opt.unwrap_or(0),
            aggregate_device_id: aggregate_device_id_opt.unwrap_or(0),
            audio_queue: queue,
            stop_flag,
        },
        source_id,
        _ctx: ctx,
    }));

    Ok(Some(AudioShareStartResult {
        loopback_exclusion_available: true,
        real_output_device_id: Some(result_uid.clone()),
        real_output_device_name: super::macos_virtual_device::get_device_name_for_uid(&result_uid),
        // The virtual-device path routes all system audio through BlackHole.
        // WebKit's AudioContext.setSinkId is unavailable, so we cannot redirect
        // room audio away from the bridge. The JS side must mute local playback.
        requires_mute_for_echo_prevention: true,
    }))
}

/// Start capturing system audio via the macOS three-tier fallback chain:
/// Core Audio process tap -> virtual device -> ScreenCaptureKit.
#[cfg(target_os = "macos")]
fn audio_share_start_macos(
    source_id: String,
    _state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
    app: tauri::AppHandle,
) -> Result<AudioShareStartResult, String> {
    use objc2::rc::Retained;
    use objc2::{AnyThread, ClassType};
    use objc2_foundation::{NSArray, NSMutableArray};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutputType,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    // ── Double-start guard ─────────────────────────────────────────
    let mut guard = audio_capture
        .active
        .lock()
        .map_err(|e| format!("audio capture lock: {e}"))?;
    if guard.is_some() {
        return Err("audio capture already active".into());
    }

    // ── Runtime macOS version check ────────────────────────────────
    let version = current_macos_version();
    if !version.supports_screen_capture_kit() {
        return Err("audio sharing requires macOS 12.3 or later".to_string());
    }

    // ── macOS 14.2+: Core Audio process tap ────────────────────────
    // Core Audio process taps exclude Wavis audio at the routing layer,
    // correctly handling WKWebView helper processes that SCK cannot exclude.
    // On newer macOS versions this can fail if "Screen & System Audio
    // Recording" permission is missing or stale, especially in dev builds.
    let tap_result = try_start_process_tap(version, || {
        audio_share_start_tap(source_id.clone(), &mut *guard, app.clone())
    })?;

    if tap_result.is_none() {
        log::info!(
            "[audio_capture] audio_share_start_macos: process tap unavailable; \
             trying virtual-device fallback"
        );
    }

    let virtual_device_result = if tap_result.is_none() {
        audio_share_start_virtual_device(source_id.clone(), &mut *guard, app.clone())?
    } else {
        None
    };

    if tap_result.is_none() && virtual_device_result.is_none() {
        log::warn!(
            "[audio_capture] audio_share_start_macos: no supported virtual device detected; \
             falling back to ScreenCaptureKit without loopback exclusion"
        );
    }

    let (decision, selected_result) =
        select_macos_audio_share_decision(tap_result, virtual_device_result);
    match decision {
        MacAudioShareDecision::Tap | MacAudioShareDecision::VirtualDevice => {
            return Ok(selected_result);
        }
        MacAudioShareDecision::ScreenCaptureKit => {}
    }

    // ── macOS 12.3+: SCK path / tap fallback ──────────────────────────────
    // SCK remains the final fallback when neither the tap path nor
    // virtual-device routing is available. It keeps share audio working, but
    // it does not provide reliable loopback isolation for WKWebView helpers.

    // ── Get SCShareableContent (async → sync bridge) ───────────────
    let (content_tx, content_rx) =
        std::sync::mpsc::channel::<Result<Retained<SCShareableContent>, String>>();

    let content_block = block2::RcBlock::new(
        move |raw_content: *mut SCShareableContent, raw_error: *mut objc2_foundation::NSError| {
            if !raw_error.is_null() {
                let _ =
                    content_tx.send(Err("screen and system audio recording permission denied \
                     — grant access in System Settings \
                     > Privacy & Security > Screen & System Audio Recording"
                        .to_string()));
                return;
            }
            if raw_content.is_null() {
                let _ = content_tx.send(Err("no shareable content available \
                     (Screen & System Audio Recording permission may be denied)"
                    .to_string()));
                return;
            }
            // SAFETY: raw_content is non-null; retain bumps the refcount.
            let retained = unsafe { Retained::retain(raw_content) };
            match retained {
                Some(r) => {
                    let _ = content_tx.send(Ok(r));
                }
                None => {
                    let _ = content_tx.send(Err("failed to retain SCShareableContent".to_string()));
                }
            }
        },
    );

    unsafe {
        let _: () = objc2::msg_send![
            SCShareableContent::class(),
            getShareableContentWithCompletionHandler: &*content_block
        ];
    }

    let content = content_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| {
            "timed out waiting for SCShareableContent \
             (Screen & System Audio Recording permission may not be granted)"
                .to_string()
        })??;

    // ── Get primary display ────────────────────────────────────────
    let displays: Retained<NSArray<objc2_screen_capture_kit::SCDisplay>> =
        unsafe { objc2::msg_send![&*content, displays] };
    let display_count: usize = unsafe { objc2::msg_send![&*displays, count] };
    if display_count == 0 {
        return Err("no displays found for audio capture".to_string());
    }
    let display: Retained<objc2_screen_capture_kit::SCDisplay> =
        unsafe { objc2::msg_send![&*displays, objectAtIndex: 0usize] };

    // ── Find the Wavis process + WebKit helpers to exclude ────────
    let my_pid = std::process::id() as i32;
    let apps: Retained<NSArray<objc2_screen_capture_kit::SCRunningApplication>> =
        unsafe { objc2::msg_send![&*content, applications] };
    let app_count: usize = unsafe { objc2::msg_send![&*apps, count] };

    let mut wavis_app_opt: Option<Retained<objc2_screen_capture_kit::SCRunningApplication>> = None;
    // Collect WebKit XPC helpers (WebContent, GPU, Networking) by bundle ID.
    // These are system processes spawned by WKWebView that play LiveKit audio
    // under a separate PID — excludesCurrentProcessAudio doesn't cover them.
    let mut webkit_apps: Vec<Retained<objc2_screen_capture_kit::SCRunningApplication>> = Vec::new();
    eprintln!(
        "[wavis-diag] sck: main PID={}, SCRunningApplications count={}",
        my_pid, app_count
    );
    for i in 0..app_count {
        let candidate: Retained<objc2_screen_capture_kit::SCRunningApplication> =
            unsafe { objc2::msg_send![&*apps, objectAtIndex: i] };
        let pid: i32 = unsafe { objc2::msg_send![&*candidate, processID] };
        let bundle_id_ns: Option<Retained<objc2_foundation::NSString>> =
            unsafe { objc2::msg_send![&*candidate, bundleIdentifier] };
        let bundle_id = bundle_id_ns
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_default();
        if i < 20 {
            eprintln!(
                "[wavis-diag] sck:   app[{}] pid={} bundle={}",
                i, pid, bundle_id
            );
        }
        if pid == my_pid {
            wavis_app_opt = Some(candidate);
        } else if bundle_id.starts_with("com.apple.WebKit.") {
            eprintln!(
                "[wavis-diag] sck:   EXCLUDING WebKit helper pid={} bundle={}",
                pid, bundle_id
            );
            webkit_apps.push(candidate);
        }
    }
    eprintln!(
        "[wavis-diag] sck: found main={} webkit_helpers={}",
        wavis_app_opt.is_some(),
        webkit_apps.len()
    );

    if wavis_app_opt.is_none() {
        log::warn!(
            "[audio_capture] sck: Wavis PID {} not found in SCRunningApplications; \
             echo exclusion via filter uses WebKit helpers only ({} matched)",
            my_pid,
            webkit_apps.len()
        );
    }

    // ── Build SCContentFilter (primary display, Wavis excluded) ───
    type SCApp = objc2_screen_capture_kit::SCRunningApplication;
    type SCWin = objc2_screen_capture_kit::SCWindow;

    let excluding: Retained<NSArray<SCApp>> = unsafe {
        match (wavis_app_opt.as_ref(), webkit_apps.is_empty()) {
            // No apps found at all — empty array, rely on excludesCurrentProcessAudio only
            (None, true) => objc2::msg_send![NSArray::<SCApp>::class(), array],

            // Main app only, no WebKit helpers registered with SCKit
            (Some(main_app), true) => objc2::msg_send![
                NSArray::<SCApp>::class(),
                arrayWithObject: &**main_app
            ],

            // WebKit helpers present — build via NSMutableArray
            _ => {
                let mut_arr: Retained<NSMutableArray<SCApp>> =
                    objc2::msg_send![NSMutableArray::<SCApp>::class(), array];
                if let Some(main_app) = wavis_app_opt.as_ref() {
                    let _: () = objc2::msg_send![&*mut_arr, addObject: &**main_app];
                }
                for child in &webkit_apps {
                    let _: () = objc2::msg_send![&*mut_arr, addObject: &**child];
                }
                // SAFETY: NSMutableArray is a subclass of NSArray
                objc2::rc::Retained::cast_unchecked::<NSArray<SCApp>>(mut_arr)
            }
        }
    };
    let excepting: Retained<NSArray<SCWin>> =
        unsafe { objc2::msg_send![NSArray::<SCWin>::class(), array] };

    let filter: Retained<SCContentFilter> = unsafe {
        objc2::msg_send![
            SCContentFilter::alloc(),
            initWithDisplay: &*display,
            excludingApplications: &*excluding,
            exceptingWindows: &*excepting
        ]
    };

    // ── Configure SCStream (audio-only, 48 kHz mono) ───────────────
    let config: Retained<SCStreamConfiguration> =
        unsafe { objc2::msg_send![SCStreamConfiguration::alloc(), init] };
    unsafe {
        // Enable audio output.
        let _: () = objc2::msg_send![&*config, setCapturesAudio: true];
        // macOS 13+: belt-and-suspenders on top of the content filter.
        let _: () = objc2::msg_send![&*config, setExcludesCurrentProcessAudio: true];
        // 48 kHz mono — matches the LiveKit ScreenShareAudio track format.
        // setSampleRate and setChannelCount expect NSInteger (i64), NOT f64.
        // Passing f64 via msg_send! puts the value in a float register (d0)
        // while the ObjC setter reads from an integer register (x2) on arm64,
        // resulting in a garbage sample rate and silent audio.
        let _: () = objc2::msg_send![&*config, setSampleRate: 48000i64];
        let _: () = objc2::msg_send![&*config, setChannelCount: 1i64];
    }

    // ── Create SCStream ────────────────────────────────────────────
    let stream: Retained<SCStream> = unsafe {
        objc2::msg_send![
            SCStream::alloc(),
            initWithFilter: &*filter,
            configuration: &*config,
            delegate: Option::<&objc2::runtime::AnyObject>::None
        ]
    };

    // ── Create audio output handler and register it ────────────────
    let stop_flag = Arc::new(AtomicBool::new(false));
    let handler = AudioOutputHandler::new(app.clone(), stop_flag.clone());

    // Cast the strongly-typed handler to a raw AnyObject pointer so we can
    // pass it as `id<SCStreamOutput>` through msg_send!.
    let output_as_any: &objc2::runtime::AnyObject = unsafe {
        &*(handler.as_ref() as *const AudioOutputHandler as *const objc2::runtime::AnyObject)
    };

    let mut add_error: *mut objc2_foundation::NSError = std::ptr::null_mut();
    let add_ok: bool = unsafe {
        objc2::msg_send![
            &*stream,
            addStreamOutput: output_as_any,
            type: SCStreamOutputType::Audio,
            sampleHandlerQueue: Option::<&objc2::runtime::AnyObject>::None,
            error: &mut add_error
        ]
    };
    if !add_ok {
        return Err(format!(
            "failed to add audio output handler to SCStream (error: {:?})",
            add_error
        ));
    }

    // ── Start capture (async → sync bridge) ───────────────────────
    let (start_tx, start_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let start_block = block2::RcBlock::new(move |raw_error: *mut objc2_foundation::NSError| {
        if raw_error.is_null() {
            let _ = start_tx.send(Ok(()));
        } else {
            let _ = start_tx.send(Err(
                "ScreenCaptureKit failed to start audio capture".to_string()
            ));
        }
    });

    unsafe {
        let _: () = objc2::msg_send![
            &*stream,
            startCaptureWithCompletionHandler: &*start_block
        ];
    }

    start_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "timed out waiting for SCStream to start".to_string())??;

    // ── Store handle ───────────────────────────────────────────────
    *guard = Some(MacAudioHandle::Sck(ScCaptureHandle {
        stream,
        stop_flag,
        source_id,
        _handler: Box::new(handler),
    }));

    log::info!(
        "[audio_capture] audio_share_start_macos: \
         capturing system audio via ScreenCaptureKit \
         (final fallback path without routing isolation)"
    );

    Ok(bare_sck_fallback_result())
}

// ── audio_share_stop_macos ────────────────────────────────────────────────

/// Stop the active audio capture session on macOS.
///
/// Works for both the SCK path (macOS 12.3–14.1) and the Core Audio process
/// tap path (macOS 14.2+).  Idempotent — returns `Ok(())` when no capture
/// is active.
#[cfg(target_os = "macos")]
fn audio_share_stop_macos(
    _state: tauri::State<'_, crate::media::MediaState>,
    audio_capture: tauri::State<'_, AudioCaptureState>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    // ── Take the handle (leaves state as None) ─────────────────────
    let handle = {
        let mut guard = audio_capture
            .active
            .lock()
            .map_err(|e| format!("audio capture lock: {e}"))?;
        guard.take()
    };

    let Some(handle) = handle else {
        return Ok(());
    };

    match handle {
        // ── SCK stop path ──────────────────────────────────────────
        MacAudioHandle::Sck(h) => {
            h.stop_flag.store(true, Ordering::Relaxed);

            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            let stop_block =
                block2::RcBlock::new(move |_raw_error: *mut objc2_foundation::NSError| {
                    let _ = stop_tx.send(());
                });

            unsafe {
                let _: () = objc2::msg_send![
                    &*h.stream,
                    stopCaptureWithCompletionHandler: &*stop_block
                ];
            }

            if stop_rx.recv_timeout(Duration::from_secs(3)).is_err() {
                log::warn!(
                    "[audio_capture] audio_share_stop_macos: \
                     SCStream stop timed out after 3 s, continuing cleanup"
                );
            }

            drop(h);
            log::info!("[audio_capture] audio_share_stop_macos: SCK capture stopped");
        }

        // ── Core Audio tap stop path ───────────────────────────────
        MacAudioHandle::Tap(h) => {
            // Signal IOProc to stop emitting frames first.
            h.stop_flag.store(true, Ordering::Relaxed);

            unsafe {
                let stop_status = AudioDeviceStop(h.tap_id, h.proc_id);
                if stop_status != 0 {
                    log::warn!(
                        "[audio_capture] audio_share_stop_macos: \
                         AudioDeviceStop failed: OSStatus {stop_status}"
                    );
                }
                let destroy_status = AudioDeviceDestroyIOProcID(h.tap_id, h.proc_id);
                if destroy_status != 0 {
                    log::warn!(
                        "[audio_capture] audio_share_stop_macos: \
                         AudioDeviceDestroyIOProcID failed: OSStatus {destroy_status}"
                    );
                }
                let tap_destroy_status = AudioHardwareDestroyProcessTap(h.tap_id);
                if tap_destroy_status != 0 {
                    log::warn!(
                        "[audio_capture] audio_share_stop_macos: \
                         AudioHardwareDestroyProcessTap failed: OSStatus {tap_destroy_status}"
                    );
                }
            }

            // Dropping h decrements the IoProcCtx Arc; ctx is released once
            // any in-flight IOProc callback completes and its reference drops.
            drop(h);
            log::info!("[audio_capture] audio_share_stop_macos: tap capture stopped");
        }

        MacAudioHandle::VirtualDevice(mut h) => {
            teardown_virtual_device_routing(
                &mut h.routing_state,
                VirtualDeviceTeardownTrigger::ManualStop,
                "explicit stop",
            );
            drop(h);
            share_audio_info!(
                "[audio_capture] audio_share_stop_macos: virtual-device capture stopped"
            );
        }
    }

    Ok(())
}
