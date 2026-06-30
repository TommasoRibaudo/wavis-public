//! Linux microphone capture via PulseAudio pa_simple.
//!
//! Replaces CPAL/ALSA mic capture on Linux to avoid the bundled libasound.so.2
//! issue in AppImage builds where snd_pcm_open("default") returns ENXIO.
//! The pa_simple API is already proven to work in AppImage via screen audio capture.

use psimple::Simple;
use pulse::def::BufferAttr;
use pulse::sample::{Format, Spec};
use pulse::stream::Direction;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wavis_client_shared::cpal_audio::AudioBuffer;

const SAMPLE_RATE: u32 = 48_000;
const FRAME_SAMPLES: usize = 960; // 20ms at 48 kHz
const FRAME_BYTES: u32 = (FRAME_SAMPLES * std::mem::size_of::<i16>()) as u32;
const PA_OPEN_ATTEMPTS: u32 = 3;
const PA_OPEN_BACKOFF: std::time::Duration = std::time::Duration::from_millis(150);

pub(crate) struct LinuxMicHandle {
    stop_flag: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl LinuxMicHandle {
    pub(crate) fn stop(self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

pub(crate) fn start_linux_mic_capture(
    capture_buffer: AudioBuffer,
    input_gain: Arc<Mutex<f32>>,
) -> Result<LinuxMicHandle, String> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_thread = Arc::clone(&stop_flag);

    let thread = std::thread::Builder::new()
        .name("pa-mic-capture".into())
        .spawn(move || mic_capture_loop(&stop_flag_thread, &capture_buffer, &input_gain))
        .map_err(|e| format!("failed to spawn pa_simple mic thread: {e}"))?;

    Ok(LinuxMicHandle { stop_flag, thread })
}

fn mic_capture_loop(
    stop_flag: &AtomicBool,
    capture_buffer: &AudioBuffer,
    input_gain: &Mutex<f32>,
) {
    let spec = Spec { format: Format::S16le, channels: 1, rate: SAMPLE_RATE };
    assert!(spec.is_valid());

    let buffer_attr = BufferAttr {
        maxlength: FRAME_BYTES * 4,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: FRAME_BYTES,
    };

    let simple = {
        let mut last_err = None;
        let mut opened = None;
        for attempt in 1..=PA_OPEN_ATTEMPTS {
            match Simple::new(
                None,
                "wavis-mic",
                Direction::Record,
                None, // default PA source — PipeWire routes to default mic
                "mic-capture",
                &spec,
                None,
                Some(&buffer_attr),
            ) {
                Ok(s) => {
                    opened = Some(s);
                    break;
                }
                Err(e) => {
                    log::warn!(
                        "[linux_mic] pa_simple open attempt {attempt}/{PA_OPEN_ATTEMPTS}: {e}"
                    );
                    last_err = Some(format!("{e}"));
                    if attempt < PA_OPEN_ATTEMPTS {
                        std::thread::sleep(PA_OPEN_BACKOFF);
                    }
                }
            }
        }
        match opened {
            Some(s) => s,
            None => {
                log::error!(
                    "[linux_mic] pa_simple mic open failed: {}",
                    last_err.unwrap_or_default()
                );
                return;
            }
        }
    };

    log::info!("[linux_mic] mic capture started (default PA source, 48kHz mono S16le)");

    let mut i16_buf = vec![0i16; FRAME_SAMPLES];
    let mut f32_buf = vec![0.0f32; FRAME_SAMPLES];

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        let byte_buf = unsafe {
            std::slice::from_raw_parts_mut(
                i16_buf.as_mut_ptr() as *mut u8,
                FRAME_SAMPLES * std::mem::size_of::<i16>(),
            )
        };

        if let Err(e) = simple.read(byte_buf) {
            log::error!("[linux_mic] pa_simple read error: {e}");
            break;
        }

        let gain = *input_gain.lock().unwrap_or_else(|e| e.into_inner());
        for (f, &s) in f32_buf.iter_mut().zip(i16_buf.iter()) {
            *f = (s as f32 / 32768.0) * gain;
        }

        capture_buffer.write_mono(&f32_buf);
    }

    log::info!("[linux_mic] mic capture stopped");
}
